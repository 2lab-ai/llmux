//! Shared OpenAI-Responses-API machinery (R5 of `docs/grok/spec.md`): the
//! provider-agnostic token estimate and the Responses-SSE → Anthropic-SSE
//! converter. Per-provider adapters — codex, grok — resolve their own
//! model/effort/headers and hand the request builder a [`RequestPlan`] plus a
//! [`ResponsesFlavor`]; behavior knobs that differ between backends live as
//! plan flags or the flavor, never as forks of the translation itself.
//!
//! Request TRANSLATION and validation live in [`super::responses_request`] and
//! are re-exported here, so `provider::responses` remains the single façade.

use serde_json::{json, Value};

use crate::proxy::sse::{SseTransform, StreamUsage};

/// Request-side surface, re-exported so `provider::responses` stays the single
/// façade every caller (proxy, adapters) imports from. The implementation —
/// and every request-shape check — lives in [`super::responses_request`]; this
/// module owns only [`RequestPlan`] (the adapter-filled knob bag), the token
/// estimate, and the response-side converter.
///
/// `build_responses_body` takes the flavor as a THIRD ARGUMENT rather than as a
/// `RequestPlan` field, so the plan stays a pure per-request knob bag and the
/// flavor cannot be silently defaulted by a caller that forgets to set it.
pub use super::responses_request::{
    build_responses_body, validate_request, CompatibilityReport, ResponsesFlavor,
};

/// Request path appended to a Responses-API upstream base URL.
pub const RESPONSES_PATH: &str = "/responses";

/// RFC-4122-shaped v4 UUID from the OS CSPRNG (no uuid crate dependency).
pub(crate) fn uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    if let Err(err) = getrandom::fill(&mut bytes) {
        // Same policy as the OAuth PKCE generator: never degrade entropy.
        panic!("OS CSPRNG unavailable: {err}");
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// The human-readable message of an `error` value in EITHER wire shape:
/// the OpenAI object (`{"message": …}`) or the xAI plain string.
fn error_message_any_shape(error: &Value) -> Option<String> {
    if let Some(s) = error.as_str() {
        return Some(s.to_string());
    }
    error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Adapter-resolved knobs for one upstream Responses body. The adapter owns
/// model + effort RESOLUTION (per-request pass-through, clamping); this module
/// owns the translation.
pub struct RequestPlan<'a> {
    /// Model slug requested upstream (already resolved by the adapter).
    pub upstream_model: &'a str,
    /// Resolved + clamped `reasoning.effort`; `None` omits the `reasoning`
    /// field entirely (the only universally-accepted wire form).
    pub effort: Option<String>,
    /// `service_tier: "priority"` (codex fast mode). Grok has no tier.
    pub priority_tier: bool,
    /// `include: ["reasoning.encrypted_content"]` (OpenAI-specific; grok off).
    pub include_encrypted_reasoning: bool,
    /// Stable per-process id sent as `prompt_cache_key` (cache hint, not
    /// conversation state).
    pub session_id: &'a str,
}

/// Total UTF-8 characters of every string anywhere under `value` (recurses
/// arrays and object values). The atom of the chars/4 token estimate.
fn section_chars(value: &Value) -> u64 {
    match value {
        Value::String(s) => s.chars().count() as u64,
        Value::Array(items) => items.iter().map(section_chars).sum(),
        Value::Object(map) => map.values().map(section_chars).sum(),
        _ => 0,
    }
}

/// Characters of the COMPACT serialized JSON of `value` — object keys,
/// delimiters and non-string scalars included, unlike [`section_chars`].
/// Key order does not change the count, so the result is stable whether or
/// not serde_json preserves insertion order.
fn serialized_chars(value: &Value) -> u64 {
    value.to_string().chars().count() as u64
}

/// chars/4 token estimate for one request section (e.g. just `system`, just
/// `tools`, or just `messages`) so the trace can report the input breakdown
/// per part. NOT floored — sum the parts, then floor the total if needed.
pub fn estimate_section_tokens(value: &Value) -> u64 {
    section_chars(value) / 4
}

/// Naive input-token estimate for `/v1/messages/count_tokens` on a Responses
/// account (no upstream equivalent): characters / 4, floor 1. A HEURISTIC,
/// not a tokenizer — every caller surfaces it as an estimate.
///
/// `system` and `messages` are prose, so the string-value atom
/// ([`section_chars`]) is a fair proxy. `tools` is not: a JSON Schema's
/// weight lives in its object KEYS (`properties`, every property name) and
/// its structure, none of which `section_chars` can see — and an agentic
/// client sends the full schema of every tool on every turn, so omitting
/// `tools` drops the dominant term of the prompt. Tools are therefore
/// counted from their serialized form ([`serialized_chars`]).
///
/// Multimodal bodies are out of scope here: base64 image payloads would be
/// counted as characters and inflate the estimate wildly, so `count_tokens`
/// owes them an explicit unsupported answer rather than a wrong number.
pub fn estimate_input_tokens(body: &Value) -> u64 {
    let mut total = 0u64;
    if let Some(system) = body.get("system") {
        total += section_chars(system);
    }
    if let Some(messages) = body.get("messages") {
        total += section_chars(messages);
    }
    if let Some(tools) = body.get("tools") {
        total += serialized_chars(tools);
    }
    (total / 4).max(1)
}

// ---------------------------------------------------------------------------
// Response conversion: Responses SSE → Anthropic SSE
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
    ToolUse,
}

/// The single byte appended to an interrupted tool call's arguments so the
/// client's accumulated `partial_json` cannot parse into anything runnable.
/// See the §3b contract in `docs/responses-compatibility/trace.md`.
const TRUNCATION_MARKER: &str = "{";

/// Whether accumulated function-call arguments form a runnable Anthropic
/// `tool_use.input`: an empty buffer (a genuinely argument-less call) or a
/// complete JSON OBJECT. Scalars and arrays are valid JSON but are NOT valid
/// `input`, so they are refused exactly as hard as truncated bytes.
fn tool_args_executable(args: &str) -> bool {
    args.is_empty()
        || serde_json::from_str::<Value>(args)
            .map(|v| v.is_object())
            .unwrap_or(false)
}

/// One upstream CONTENT PART's accumulated content. Blocks are keyed by
/// upstream identity, not by arrival order: a part keeps its own buffer while
/// a different one holds the single Anthropic wire slot, so interleaved
/// streams never lose or merge content.
///
/// Identity has TWO axes, because one item can carry several parts whose
/// deltas interleave: the item (`item_id` / `item.id` / `output_index`) and
/// the part within it (`content_index`, or `summary_index` for reasoning).
/// `key` is the full part identity and routes deltas; `item` is the
/// part-independent half and is what `response.output_item.done` finishes.
#[derive(Debug)]
struct AggBlock {
    kind: BlockKind,
    /// Full part identity (`{item}#{part}`). `None` for legacy captures that
    /// carry no identity at all.
    key: Option<String>,
    /// Item identity without the part suffix, for item-level lifecycle events.
    item: Option<String>,
    text: String,
    tool_id: String,
    tool_name: String,
    tool_args: String,
    /// Anthropic index of the wire block currently carrying this item.
    /// `None` while the item is buffered or already closed.
    wire_index: Option<usize>,
    /// Whether this block has EVER held the wire. Set in `start_wire` and
    /// never reset — `wire_index` is cleared on close, so it cannot tell "not
    /// yet opened" from "already finished", and a block that never opens must
    /// stay pending even when it has no bytes to pump.
    wired: bool,
    /// Bytes of the payload already emitted as deltas. Deltas are appended as
    /// whole strings, so this is always on a char boundary.
    emitted: usize,
    /// Upstream `output_item.done` seen — no further content will arrive.
    done: bool,
}

impl AggBlock {
    /// The payload this block streams: arguments for a tool, text otherwise.
    fn payload(&self) -> &str {
        match self.kind {
            BlockKind::ToolUse => &self.tool_args,
            _ => &self.text,
        }
    }

    /// Payload not yet sent to the client as a delta.
    fn unemitted(&self) -> &str {
        let payload = self.payload();
        &payload[self.emitted.min(payload.len())..]
    }
}

/// Stateful Responses→Anthropic SSE converter. One instance per upstream
/// response; feed COMPLETE upstream events in, get well-formed Anthropic SSE
/// bytes out (`event: <type>\ndata: <json>\n\n`, indexes sequenced).
#[derive(Debug)]
pub struct ResponsesSseConverter {
    /// Real codex model slug (the value requested upstream). Kept for internal
    /// use; the client-facing stamp prefers `client_model` when set.
    model: String,
    /// Optional override for the model NAME stamped into the client-facing
    /// Anthropic `message_start` / aggregate. When `Some`, Claude Code sees
    /// this instead of `model` (so its hardcoded context-window lookup picks a
    /// 1M denominator); routing/dashboard/trace still use `model`. `None`
    /// (default) → stamp the real `model`.
    client_model: Option<String>,
    /// Short provider tag stamped into synthesized message ids
    /// (`msg_{tag}_…`) and diagnostic logs. `"codex"` is the historical
    /// default (codex was the first Responses backend); grok passes its own
    /// via [`Self::with_tag`].
    tag: &'static str,
    started: bool,
    finished: bool,
    message_id: String,
    next_index: usize,
    /// Position in `blocks` of the item currently holding the single open
    /// Anthropic content block. Anthropic SSE allows exactly one open block at
    /// a time, so every other item's content stays buffered in its `AggBlock`
    /// until this slot frees up — it is NEVER appended to whichever block
    /// happens to be last.
    open_pos: Option<usize>,
    saw_tool_use: bool,
    /// `usage.input_tokens` is the FRESH (non-cached) prompt count, matching
    /// Anthropic's convention; the cached subset lives in
    /// `cached_input_tokens`. OpenAI Responses reports the cache-INCLUSIVE
    /// total, so `complete()` subtracts the cached part — otherwise the
    /// dashboard counts cached tokens that the Claude side never counts (≈90×
    /// inflation) and the client's context bar fills on cache reads.
    usage: StreamUsage,
    cached_input_tokens: u64,
    blocks: Vec<AggBlock>,
    stop_reason: Option<String>,
    error: Option<String>,
    /// Verbatim upstream `usage` object from `response.completed`, kept for the
    /// codex trace (input_tokens / input_tokens_details.cached_tokens /
    /// output_tokens / output_tokens_details.reasoning_tokens / total_tokens) —
    /// the reduced `StreamUsage` drops the reasoning + total splits we want to
    /// diagnose token issues from the log.
    raw_usage: Option<Value>,
    /// Count of upstream SSE events parsed (any `data:` event), so the trace
    /// can show whether the stream produced events at all vs. hung.
    events_seen: u64,
}

impl Default for ResponsesSseConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponsesSseConverter {
    /// Converter stamping the fallback [`CODEX_MODEL`]. Used by tests; the
    /// provider uses [`ResponsesSseConverter::with_model`].
    pub fn new() -> Self {
        Self::with_model(super::codex::CODEX_MODEL.to_string())
    }

    /// Converter that stamps `model` into the synthesized Anthropic response.
    pub fn with_model(model: String) -> Self {
        Self {
            model,
            client_model: None,
            tag: "codex",
            started: false,
            finished: false,
            message_id: String::new(),
            next_index: 0,
            open_pos: None,
            saw_tool_use: false,
            usage: StreamUsage::default(),
            cached_input_tokens: 0,
            blocks: Vec::new(),
            stop_reason: None,
            error: None,
            raw_usage: None,
            events_seen: 0,
        }
    }

    /// Set the optional client-facing model-name override (see
    /// [`Self::client_model`]). Builder-style so the provider can chain it
    /// after [`Self::with_model`].
    pub fn with_client_model(mut self, client_model: Option<String>) -> Self {
        self.client_model = client_model;
        self
    }

    /// Set the provider tag (message-id prefix + log field). Builder-style,
    /// like [`Self::with_client_model`].
    pub fn with_tag(mut self, tag: &'static str) -> Self {
        self.tag = tag;
        self
    }

    /// The model NAME to stamp into client-facing responses: the override when
    /// set, else the real model. Only the two response stamps use this; every
    /// internal path (routing, dashboard, trace, scheduler) reads `model`.
    fn client_facing_model(&self) -> &str {
        self.client_model.as_deref().unwrap_or(&self.model)
    }

    fn emit(out: &mut Vec<u8>, event_type: &str, data: &Value) {
        out.extend_from_slice(format!("event: {event_type}\ndata: {data}\n\n").as_bytes());
    }

    fn ensure_started(&mut self, out: &mut Vec<u8>) {
        if self.started {
            return;
        }
        self.started = true;
        if self.message_id.is_empty() {
            self.message_id = format!(
                "msg_{}_{}",
                self.tag,
                ulid::Ulid::new().to_string().to_lowercase()
            );
        }
        Self::emit(
            out,
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.client_facing_model(),
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": 0, "output_tokens": 0},
                },
            }),
        );
    }

    /// Identity of the upstream output ITEM an event belongs to: the delta
    /// events carry a top-level `item_id`, the lifecycle events carry
    /// `item.id`, and both carry `output_index`. Captures predating
    /// `output_index` carry neither and yield `None`, which routes by "whatever
    /// holds the wire" — the pre-identity behavior.
    fn item_key(value: &Value) -> Option<String> {
        if let Some(id) = value.get("item_id").and_then(Value::as_str) {
            return Some(format!("id:{id}"));
        }
        if let Some(id) = value
            .get("item")
            .and_then(|i| i.get("id"))
            .and_then(Value::as_str)
        {
            return Some(format!("id:{id}"));
        }
        value
            .get("output_index")
            .and_then(Value::as_u64)
            .map(|i| format!("idx:{i}"))
    }

    /// Index of the content PART within its item: `content_index` for message
    /// content, `summary_index` for reasoning summaries (the analogue). Events
    /// that address the item as a whole — `output_item.added/done` — carry
    /// neither and address part 0, the part every item starts with.
    fn part_index(value: &Value) -> u64 {
        value
            .get("content_index")
            .or_else(|| value.get("summary_index"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }

    /// Full part identity, or `None` when the event carries no item identity
    /// at all (legacy captures route by the wire instead).
    fn part_key(value: &Value) -> Option<String> {
        Self::item_key(value).map(|item| format!("{item}#{}", Self::part_index(value)))
    }

    fn position_of(&self, key: &str, kind: BlockKind) -> Option<usize> {
        self.blocks
            .iter()
            .position(|b| b.key.as_deref() == Some(key) && b.kind == kind)
    }

    fn push_block(&mut self, key: Option<String>, item: Option<String>, kind: BlockKind) -> usize {
        self.blocks.push(AggBlock {
            kind,
            key,
            item,
            text: String::new(),
            tool_id: String::new(),
            tool_name: String::new(),
            tool_args: String::new(),
            wire_index: None,
            wired: false,
            emitted: 0,
            done: false,
        });
        self.blocks.len() - 1
    }

    /// The block accumulating a DELTA for part `key`. Without identity, reuse
    /// the block on the wire when its kind matches (legacy behavior) and
    /// otherwise start a new one.
    fn route(&mut self, key: Option<&str>, item: Option<&str>, kind: BlockKind) -> usize {
        if let Some(key) = key {
            return match self.position_of(key, kind) {
                Some(pos) => pos,
                None => self.push_block(Some(key.to_string()), item.map(str::to_string), kind),
            };
        }
        if let Some(open) = self.open_pos {
            if self.blocks[open].kind == kind {
                return open;
            }
        }
        self.push_block(None, None, kind)
    }

    /// The block for a newly ANNOUNCED item (`response.output_item.added`):
    /// its part 0, created fresh unless this exact part was already seen.
    fn route_new(&mut self, key: Option<&str>, item: Option<&str>, kind: BlockKind) -> usize {
        match key.and_then(|key| self.position_of(key, kind)) {
            Some(pos) => pos,
            None => self.push_block(key.map(str::to_string), item.map(str::to_string), kind),
        }
    }

    /// Give block `pos` the single open wire slot. Caller guarantees it is free.
    fn start_wire(&mut self, out: &mut Vec<u8>, pos: usize) {
        let index = self.next_index;
        self.next_index += 1;
        self.blocks[pos].wire_index = Some(index);
        self.blocks[pos].wired = true;
        self.open_pos = Some(pos);
        let block = &self.blocks[pos];
        let content_block = match block.kind {
            BlockKind::Text => json!({"type": "text", "text": ""}),
            BlockKind::Thinking => json!({"type": "thinking", "thinking": ""}),
            BlockKind::ToolUse => json!({
                "type": "tool_use",
                "id": block.tool_id,
                "name": block.tool_name,
                "input": {},
            }),
        };
        Self::emit(
            out,
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": index,
                "content_block": content_block,
            }),
        );
    }

    /// Emit block `pos`'s un-emitted payload tail — but only while it holds the
    /// wire. Anything else stays buffered until [`Self::flush_pending`] gives
    /// it the wire; it is never dropped and never merged into another block.
    fn pump(&mut self, out: &mut Vec<u8>, pos: usize) {
        if self.open_pos != Some(pos) {
            return;
        }
        let Some(index) = self.blocks[pos].wire_index else {
            return;
        };
        let tail = self.blocks[pos].unemitted().to_string();
        if tail.is_empty() {
            return;
        }
        let delta = match self.blocks[pos].kind {
            BlockKind::Text => json!({"type": "text_delta", "text": tail}),
            BlockKind::Thinking => json!({"type": "thinking_delta", "thinking": tail}),
            BlockKind::ToolUse => json!({"type": "input_json_delta", "partial_json": tail}),
        };
        self.blocks[pos].emitted += tail.len();
        self.delta(out, index, delta);
    }

    /// Flush and close whatever holds the wire.
    fn close_wire(&mut self, out: &mut Vec<u8>) {
        let Some(pos) = self.open_pos else {
            return;
        };
        self.pump(out, pos);
        if let Some(index) = self.blocks[pos].wire_index.take() {
            Self::emit(
                out,
                "content_block_stop",
                &json!({"type": "content_block_stop", "index": index}),
            );
        }
        self.open_pos = None;
    }

    /// Put `pos` on the wire if the slot is free, then emit its tail.
    fn wire_and_pump(&mut self, out: &mut Vec<u8>, pos: usize) {
        if self.open_pos.is_none() {
            self.start_wire(out, pos);
        }
        self.pump(out, pos);
    }

    /// Hand the wire to a newly announced item, preempting the current holder
    /// — UNLESS that holder is a tool block. A tool call must never be split
    /// across two Anthropic blocks: two halves of one call are two malformed
    /// calls. Text and thinking split harmlessly (clients concatenate text
    /// blocks, and the aggregate re-joins by item), so they yield.
    fn claim_wire(&mut self, out: &mut Vec<u8>, pos: usize) {
        if let Some(open) = self.open_pos {
            if open == pos {
                return;
            }
            if self.blocks[open].kind == BlockKind::ToolUse {
                return; // stays buffered until the tool finishes
            }
            self.close_wire(out);
        }
        self.start_wire(out, pos);
    }

    /// Position of the first block still owed to the client: one that has
    /// never reached the wire, or one holding un-emitted bytes.
    ///
    /// "Never wired" is load-bearing. A block whose whole payload is empty —
    /// an argument-less `function_call` buffered behind another live tool —
    /// has nothing to pump, so a bytes-only predicate skips it forever: it
    /// never reaches the stream while `into_message_json` still lists it,
    /// telling a non-streaming client about a tool call a streaming client
    /// never saw.
    fn next_pending(&self) -> Option<usize> {
        (0..self.blocks.len())
            .find(|&i| !self.blocks[i].wired || !self.blocks[i].unemitted().is_empty())
    }

    /// Drain buffered items onto the wire in upstream item order, for as long
    /// as the slot is free. An item that upstream has already finished is
    /// closed immediately; one still receiving deltas is left open for them.
    fn flush_pending(&mut self, out: &mut Vec<u8>) {
        while self.open_pos.is_none() {
            let Some(pos) = self.next_pending() else {
                return;
            };
            self.start_wire(out, pos);
            self.pump(out, pos);
            if self.blocks[pos].done {
                self.close_wire(out);
            } else {
                return;
            }
        }
    }

    /// Terminal drain: every item's remaining payload reaches the wire and
    /// every opened block closes, before the `message_delta` tail.
    fn flush_all(&mut self, out: &mut Vec<u8>) {
        self.close_wire(out);
        while let Some(pos) = self.next_pending() {
            self.start_wire(out, pos);
            self.pump(out, pos);
            self.close_wire(out);
        }
    }

    fn delta(&mut self, out: &mut Vec<u8>, index: usize, delta: Value) {
        Self::emit(
            out,
            "content_block_delta",
            &json!({"type": "content_block_delta", "index": index, "delta": delta}),
        );
    }

    fn fail(&mut self, out: &mut Vec<u8>, message: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.error = Some(message.to_string());
        Self::emit(
            out,
            "error",
            &json!({
                "type": "error",
                "error": {"type": "api_error", "message": message},
            }),
        );
    }

    /// Terminal Anthropic tail (`message_delta` + `message_stop`) for one
    /// upstream response, folding its `usage`. `forced_stop` overrides the
    /// derived stop reason — [`Self::incomplete`] uses it so an output-cap
    /// hit reports `max_tokens` even when the turn also produced tool calls.
    /// Applying it HERE (the single place a stop reason is chosen) is what
    /// keeps the streamed `message_delta` and the non-stream aggregate from
    /// ever disagreeing.
    fn complete(&mut self, out: &mut Vec<u8>, response: Option<&Value>, forced_stop: Option<&str>) {
        if self.finished {
            return;
        }
        self.ensure_started(out);
        match forced_stop {
            // The upstream itself called this turn COMPLETE, so nothing
            // truncated it: arguments that are not a runnable object mean the
            // protocol was violated. Dropping the block here would ship a
            // `stop_reason: "tool_use"` turn with the tool silently missing —
            // a lie about a finished response — so the whole response fails,
            // identically on both legs.
            None => {
                if let Some(pos) = self.blocks.iter().position(|b| {
                    b.kind == BlockKind::ToolUse && !tool_args_executable(&b.tool_args)
                }) {
                    let tool = self.blocks[pos].tool_name.clone();
                    self.fail(
                        out,
                        &format!(
                            "upstream tool call {tool:?} sent malformed arguments \
                             (not a JSON object)"
                        ),
                    );
                    return;
                }
            }
            // Capped turn: the interrupted call must not be able to
            // materialize as a runnable `{}` on the wire either.
            Some(_) => self.mark_interrupted_tools(),
        }
        self.flush_all(out);
        if let Some(usage) = response.and_then(|r| r.get("usage")) {
            // Keep the verbatim upstream usage for the codex trace before we
            // reduce it (the trace wants reasoning + total splits too).
            self.raw_usage = Some(usage.clone());
            // OpenAI `input_tokens` is the cache-INCLUSIVE total; the cached
            // subset is `input_tokens_details.cached_tokens`. Record fresh =
            // total − cached so codex is comparable to the Anthropic side
            // (which already counts uncached input only), preserving the
            // invariant `total_input == fresh input + cache_read`.
            let total_input = usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let details = usage.get("input_tokens_details");
            // `cached` is `Some` only when the upstream reported the field, so
            // the dashboard renders unavailable (not 0) when it is absent.
            // Clamp to the total: a cache read can never exceed the tokens
            // actually received (guards a malformed `cached > input` payload) —
            // and a payload that actually violates the invariant is worth
            // operator eyes, so the clamp warns with the raw values instead of
            // silently rewriting them.
            let cached = details
                .and_then(|d| d.get("cached_tokens"))
                .and_then(Value::as_u64)
                .map(|c| {
                    if c > total_input {
                        tracing::warn!(
                            cached_tokens = c,
                            input_tokens = total_input,
                            provider = self.tag,
                            "malformed upstream usage (cached_tokens > input_tokens); clamping"
                        );
                    }
                    c.min(total_input)
                });
            // OpenAI carries the cache-WRITE subset in the same details object.
            // It is 0 on today's wire, but map it to `cache_creation_input_tokens`
            // so the dashboard's cache split stays correct if it ever isn't.
            // Codex bills nothing for cache creation, so pricing scores it at 0.
            let cache_write = details
                .and_then(|d| d.get("cache_write_tokens"))
                .and_then(Value::as_u64);
            self.cached_input_tokens = cached.unwrap_or(0);
            self.usage = StreamUsage {
                input_tokens: total_input.saturating_sub(self.cached_input_tokens),
                output_tokens: usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                cache_read_input_tokens: cached,
                cache_creation_input_tokens: cache_write,
            };
        }
        let stop_reason = match forced_stop {
            Some(reason) => reason,
            None if self.saw_tool_use => "tool_use",
            None => "end_turn",
        };
        self.stop_reason = Some(stop_reason.to_string());
        Self::emit(
            out,
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": {
                    "input_tokens": self.usage.input_tokens,
                    "cache_read_input_tokens": self.cached_input_tokens,
                    "cache_creation_input_tokens":
                        self.usage.cache_creation_input_tokens.unwrap_or(0),
                    "output_tokens": self.usage.output_tokens,
                },
            }),
        );
        Self::emit(out, "message_stop", &json!({"type": "message_stop"}));
        self.finished = true;
    }

    /// Terminal handling for an upstream response whose `status` is
    /// `incomplete`; the API reports WHY in `incomplete_details.reason`.
    ///
    /// `max_output_tokens` is exactly Anthropic's `stop_reason: "max_tokens"`
    /// — the turn ended for a known, benign reason and the usage it carries is
    /// real, so it ends the stream cleanly (and is NOT scored as a provider
    /// failure). Every other reason (today only `content_filter`) has no
    /// faithful Anthropic stop reason: reporting it as a finished turn would
    /// present a censored or aborted response as a complete one, so it becomes
    /// an error naming the reason. An unrecognized future reason takes the same
    /// path — unknown means error, never a synthesized success.
    fn incomplete(&mut self, out: &mut Vec<u8>, response: Option<&Value>) {
        let reason = response
            .and_then(|r| r.get("incomplete_details"))
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str);
        match reason {
            Some("max_output_tokens") => self.complete(out, response, Some("max_tokens")),
            Some(other) => self.fail(out, &format!("upstream response incomplete: {other}")),
            None => self.fail(out, "upstream response incomplete: reason missing"),
        }
    }

    /// A capped turn's UNFINISHED tool calls must not be able to materialize as
    /// runnable `{}` on the client side. When a call's accumulated arguments
    /// would still parse into something — an empty buffer, or a scalar/array
    /// that is valid JSON but not a valid `tool_use.input` — append one `{` so
    /// the accumulation is unparseable for every client.
    ///
    /// The marker goes into the BUFFER, not straight onto the wire, so the
    /// ordinary `pump` carries it whenever the block reaches the wire. That is
    /// what covers calls still BUFFERED behind another tool when the cap fired:
    /// the terminal flush wires them, and without a marker already in place
    /// they would be emitted as pristine, runnable `input: {}`.
    ///
    /// A call closed by its own `output_item.done` before the cap is untouched:
    /// it really finished, so a genuinely argument-less one stays `{}`.
    fn mark_interrupted_tools(&mut self) {
        for pos in 0..self.blocks.len() {
            if self.blocks[pos].kind != BlockKind::ToolUse || self.blocks[pos].done {
                continue;
            }
            let needed = match serde_json::from_str::<Value>(&self.blocks[pos].tool_args) {
                // Parses, but only an object is a runnable `input`.
                Ok(value) => !value.is_object(),
                // Unparseable bytes are already inert; only "nothing streamed
                // at all" can still be read as `{}`.
                Err(_) => self.blocks[pos].tool_args.is_empty(),
            };
            if needed {
                self.blocks[pos].tool_args.push_str(TRUNCATION_MARKER);
            }
        }
    }

    /// The Anthropic `input` for a tool block, or `None` when the block must
    /// not be surfaced to the client as a runnable call at all.
    fn aggregate_tool_input(block: &AggBlock) -> Option<Value> {
        if block.tool_args.is_empty() {
            // Upstream closed the item with no arguments: a genuine no-arg
            // call. An INTERRUPTED call never reaches here with an empty
            // buffer — the capped path put a truncation marker in it first, so
            // it falls through to the parse below and is refused.
            return Some(json!({}));
        }
        match serde_json::from_str::<Value>(&block.tool_args) {
            Ok(input) if input.is_object() => Some(input),
            _ => None,
        }
    }

    /// Build the single (non-streaming) Anthropic Messages response from the
    /// fully consumed stream. `None` when the upstream reported an error —
    /// callers should surface [`Self::error_message`] instead.
    pub fn into_message_json(self) -> Option<Value> {
        if self.error.is_some() {
            return None;
        }
        let content: Vec<Value> = self
            .blocks
            .iter()
            .filter_map(|block| match block.kind {
                BlockKind::Text => Some(json!({"type": "text", "text": block.text})),
                BlockKind::Thinking => Some(json!({"type": "thinking", "thinking": block.text})),
                BlockKind::ToolUse => {
                    // Reaching here on a NORMAL completion means the arguments
                    // already passed `tool_args_executable` in `complete` — a
                    // malformed one failed the whole response. So the only
                    // drops here are interrupted calls on a capped turn, which
                    // must never be surfaced as runnable. (The streaming leg
                    // cannot retract what it already sent, so this is the one
                    // place the two legs differ in CONTENT — never in
                    // `stop_reason`.)
                    let Some(input) = Self::aggregate_tool_input(block) else {
                        tracing::warn!(
                            provider = self.tag,
                            tool = %block.tool_name,
                            arg_bytes = block.tool_args.len(),
                            "dropping non-executable tool call from the aggregate"
                        );
                        return None;
                    };
                    Some(json!({
                        "type": "tool_use",
                        "id": block.tool_id,
                        "name": block.tool_name,
                        "input": input,
                    }))
                }
            })
            .collect();
        Some(json!({
            "id": if self.message_id.is_empty() {
                format!("msg_{}_{}", self.tag, ulid::Ulid::new().to_string().to_lowercase())
            } else {
                self.message_id.clone()
            },
            "type": "message",
            "role": "assistant",
            "model": self.client_facing_model(),
            "content": content,
            "stop_reason": self.stop_reason.as_deref().unwrap_or("end_turn"),
            "stop_sequence": null,
            "usage": {
                "input_tokens": self.usage.input_tokens,
                "cache_read_input_tokens": self.cached_input_tokens,
                "cache_creation_input_tokens":
                    self.usage.cache_creation_input_tokens.unwrap_or(0),
                "output_tokens": self.usage.output_tokens,
            },
        }))
    }

    /// Upstream error message, when the stream ended in `response.failed` /
    /// `error`.
    pub fn error_message(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Verbatim upstream `usage` object captured at `response.completed`, for
    /// the codex trace. `None` until a `response.completed` carrying `usage`
    /// has been folded.
    pub fn raw_usage(&self) -> Option<&Value> {
        self.raw_usage.as_ref()
    }

    /// Count of real upstream SSE events parsed so far (keepalives, `[DONE]`,
    /// and unparseable lines excluded).
    pub fn events_seen(&self) -> u64 {
        self.events_seen
    }

    /// Concatenated `data:` payload of one SSE event (Responses events are
    /// single-line JSON in practice; multi-line data is joined per the SSE
    /// spec).
    fn event_data(event: &str) -> Option<String> {
        let lines: Vec<&str> = event
            .lines()
            .filter_map(|line| {
                line.strip_prefix("data: ")
                    .or_else(|| line.strip_prefix("data:"))
            })
            .collect();
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }
}

impl SseTransform for ResponsesSseConverter {
    fn on_event(&mut self, event: &str) -> Vec<u8> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        let Some(data) = Self::event_data(event) else {
            return out; // comment/keepalive lines
        };
        if data.trim() == "[DONE]" {
            return out;
        }
        let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
            tracing::debug!(provider = self.tag, "unparseable upstream SSE data dropped");
            return out;
        };
        // One real upstream event parsed (keepalives / [DONE] / unparseable
        // lines already returned above) — surfaced in the codex trace.
        self.events_seen += 1;
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                event
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("event: ")
                            .or_else(|| l.strip_prefix("event:"))
                    })
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_default();

        match event_type.as_str() {
            "response.created" => {
                if let Some(id) = value
                    .get("response")
                    .and_then(|r| r.get("id"))
                    .and_then(Value::as_str)
                {
                    self.message_id = id.to_string();
                }
                // Adopt the upstream-reported model as the real model: with
                // per-request model pass-through the request may name a
                // different slug than the configured pin, and the upstream
                // response is the single source of truth. `client_model`
                // (when set) still wins for the client-facing stamp.
                if let Some(m) = value
                    .get("response")
                    .and_then(|r| r.get("model"))
                    .and_then(Value::as_str)
                {
                    if !m.is_empty() {
                        self.model = m.to_string();
                    }
                }
                self.ensure_started(&mut out);
            }
            "response.output_item.added" => {
                self.ensure_started(&mut out);
                // An item announcement addresses the item's part 0.
                let key = Self::part_key(&value);
                let item_key = Self::item_key(&value);
                let item = value.get("item");
                match item.and_then(|i| i.get("type")).and_then(Value::as_str) {
                    Some("message") => {
                        let pos =
                            self.route_new(key.as_deref(), item_key.as_deref(), BlockKind::Text);
                        self.claim_wire(&mut out, pos);
                    }
                    Some("function_call") => {
                        let call_id = item
                            .and_then(|i| i.get("call_id"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let name = item
                            .and_then(|i| i.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        self.saw_tool_use = true;
                        let pos =
                            self.route_new(key.as_deref(), item_key.as_deref(), BlockKind::ToolUse);
                        self.blocks[pos].tool_id = call_id;
                        self.blocks[pos].tool_name = name;
                        self.claim_wire(&mut out, pos);
                    }
                    _ => {} // reasoning items etc. open lazily via their deltas
                }
            }
            "response.output_text.delta" => {
                self.ensure_started(&mut out);
                let key = Self::part_key(&value);
                let item_key = Self::item_key(&value);
                let text = value.get("delta").and_then(Value::as_str).unwrap_or("");
                let pos = self.route(key.as_deref(), item_key.as_deref(), BlockKind::Text);
                // Upstream text is relayed VERBATIM — appended byte for byte,
                // no regex, no scrub. Anything that looks like an internal
                // artifact is model output and ships unchanged.
                self.blocks[pos].text.push_str(text);
                self.wire_and_pump(&mut out, pos);
            }
            "response.reasoning_summary_text.delta" => {
                self.ensure_started(&mut out);
                let key = Self::part_key(&value);
                let item_key = Self::item_key(&value);
                let text = value.get("delta").and_then(Value::as_str).unwrap_or("");
                let pos = self.route(key.as_deref(), item_key.as_deref(), BlockKind::Thinking);
                self.blocks[pos].text.push_str(text);
                self.wire_and_pump(&mut out, pos);
            }
            // Part-level completion: finish THAT part so a buffered sibling
            // part can start streaming immediately, instead of everything
            // draining at the terminal event.
            "response.content_part.done" | "response.reasoning_summary_part.done" => {
                let Some(key) = Self::part_key(&value) else {
                    return out; // legacy capture: item-level `done` handles it
                };
                let Some(pos) = self
                    .blocks
                    .iter()
                    .position(|b| b.key.as_deref() == Some(&key))
                else {
                    return out;
                };
                self.blocks[pos].done = true;
                if self.open_pos == Some(pos) {
                    self.close_wire(&mut out);
                }
                self.flush_pending(&mut out);
            }
            "response.function_call_arguments.delta" => {
                // A function_call item has no content parts, so its arguments
                // always address part 0.
                let key = Self::part_key(&value);
                // Accumulation is routed by IDENTITY and never depends on
                // which block currently holds the wire — that dependency is
                // exactly how interleaved argument bytes used to be dropped.
                let pos = match key.as_deref() {
                    Some(key) => match self.position_of(key, BlockKind::ToolUse) {
                        Some(pos) => pos,
                        None => {
                            // Arguments for an item we never saw announced.
                            // Inventing a `tool_use` with an empty id/name
                            // would be worse than losing them.
                            tracing::debug!(
                                provider = self.tag,
                                item = key,
                                "argument delta for an unknown item dropped"
                            );
                            return out;
                        }
                    },
                    // Legacy captures carry no identity: the live tool block is
                    // the only possible target, exactly as before.
                    None => match self.open_pos {
                        Some(open) if self.blocks[open].kind == BlockKind::ToolUse => open,
                        _ => return out,
                    },
                };
                let partial = value.get("delta").and_then(Value::as_str).unwrap_or("");
                self.blocks[pos].tool_args.push_str(partial);
                self.wire_and_pump(&mut out, pos);
            }
            "response.output_item.done" => {
                let item_key = Self::item_key(&value);
                // Resolve the FINISHED item, not "whatever is open": with
                // interleaved items a `done` routinely arrives for an item that
                // is not the live one, and closing the live block there would
                // truncate someone else's call. An item finishing finishes
                // EVERY part it carries.
                let positions: Vec<usize> = match item_key.as_deref() {
                    Some(item) => (0..self.blocks.len())
                        .filter(|&i| self.blocks[i].item.as_deref() == Some(item))
                        .collect(),
                    None => self.open_pos.into_iter().collect(),
                };
                if positions.is_empty() {
                    // No block for this item (e.g. a reasoning item that never
                    // emitted a summary) — nothing to finish.
                    return out;
                }
                // A function_call item may deliver its full arguments only
                // here, with no deltas streamed at all.
                if let Some(&pos) = positions
                    .iter()
                    .find(|&&i| self.blocks[i].kind == BlockKind::ToolUse)
                {
                    if self.blocks[pos].tool_args.is_empty() {
                        if let Some(arguments) = value
                            .get("item")
                            .and_then(|i| i.get("arguments"))
                            .and_then(Value::as_str)
                            .filter(|a| !a.is_empty())
                        {
                            self.blocks[pos].tool_args.push_str(arguments);
                        }
                    }
                }
                for &pos in &positions {
                    self.blocks[pos].done = true;
                }
                if self.open_pos.is_some_and(|open| positions.contains(&open)) {
                    self.close_wire(&mut out);
                }
                self.flush_pending(&mut out);
            }
            "response.completed" => {
                let response = value.get("response");
                // A `response.completed` envelope carrying `status:
                // "incomplete"` contradicts itself; the status is the truth
                // about what happened to the turn, so it wins.
                if response
                    .and_then(|r| r.get("status"))
                    .and_then(Value::as_str)
                    == Some("incomplete")
                {
                    self.incomplete(&mut out, response);
                } else {
                    self.complete(&mut out, response, None);
                }
            }
            "response.incomplete" => {
                self.incomplete(&mut out, value.get("response"));
            }
            "response.failed" => {
                // `error` may be the OpenAI object shape ({message}) or the
                // xAI string shape (external review N4; same class as the
                // HTTP-error condense fix — never flatten a string error to
                // a generic message).
                let message = value
                    .get("response")
                    .and_then(|r| r.get("error"))
                    .and_then(error_message_any_shape)
                    .unwrap_or_else(|| "upstream response failed".to_string());
                self.fail(&mut out, &message);
            }
            "error" => {
                let message = value
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| value.get("error").and_then(error_message_any_shape))
                    .unwrap_or_else(|| "upstream error".to_string());
                self.fail(&mut out, &message);
            }
            // in_progress / content_part / output_text.done / reasoning
            // bookkeeping events carry nothing the Anthropic stream needs.
            _ => {}
        }
        out
    }

    fn on_end(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.finished {
            // Never-started covers a 2xx whose body was not SSE at all
            // (e.g. a plain JSON document): relay_codex trusts every 2xx to
            // be a stream, so the converter must terminate it with a clean
            // Anthropic error event rather than ending silently.
            let message = if self.started {
                "upstream stream ended before response.completed"
            } else {
                "codex upstream returned no SSE events"
            };
            self.fail(&mut out, message);
        }
        out
    }

    fn usage(&self) -> StreamUsage {
        self.usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- count_tokens estimate ----

    #[test]
    fn estimate_input_tokens_counts_serialized_tool_schemas() {
        // Hand-derived (docs/responses-compatibility/trace.md, T1 worked
        // example): system "abcd" = 4 chars; the message's string VALUES
        // "user" + "efghijkl" = 12; tools serialized as `[{"name":"ls"}]` =
        // 15. 31 / 4 = 7. Ignoring `tools` (the bug) yields 4.
        let body = json!({
            "system": "abcd",
            "messages": [{"role": "user", "content": "efghijkl"}],
            "tools": [{"name": "ls"}],
        });
        assert_eq!(estimate_input_tokens(&body), 7);
    }

    #[test]
    fn estimate_input_tokens_counts_tool_property_names() {
        // Both schemas carry IDENTICAL string values ("t", "object",
        // "string"); they differ only in a property NAME, which is an object
        // KEY and therefore invisible to the string-values-only atom. The
        // name grows from 1 to 21 characters — exactly 20 more characters,
        // 5 more tokens — and a schema's property names are real prompt
        // tokens the user pays for.
        let short = json!({"tools": [{"name": "t", "input_schema":
            {"type": "object", "properties": {"a": {"type": "string"}}}}]});
        let long = json!({"tools": [{"name": "t", "input_schema":
            {"type": "object", "properties": {"abcdefghijklmnopqrstu": {"type": "string"}}}}]});
        assert_eq!(
            estimate_input_tokens(&long) - estimate_input_tokens(&short),
            5,
            "20 characters of property name = 5 tokens"
        );
    }

    #[test]
    fn estimate_input_tokens_floor_and_toolless_bodies_unchanged() {
        // Regression: adding the tools term must not move the numbers that
        // tool-less callers already see.
        assert_eq!(estimate_input_tokens(&json!({})), 1, "floor of 1");
        let toolless = json!({
            "system": "abcd",
            "messages": [{"role": "user", "content": "efghijkl"}],
        });
        assert_eq!(estimate_input_tokens(&toolless), 4);
    }

    // ---- terminal output-limit semantics ----

    fn event(json: &Value) -> String {
        format!(
            "event: {}\ndata: {json}",
            json["type"].as_str().unwrap_or("message")
        )
    }

    /// Feed a scripted upstream sequence through a fresh converter and split
    /// the emitted bytes back into `(event_type, data)` pairs.
    fn run(events: &[Value]) -> (ResponsesSseConverter, Vec<(String, Value)>) {
        let mut converter = ResponsesSseConverter::new();
        let mut emitted = Vec::new();
        for e in events {
            emitted.extend_from_slice(&converter.on_event(&event(e)));
        }
        emitted.extend_from_slice(&converter.on_end());
        let text = String::from_utf8(emitted).expect("utf8");
        let mut parsed = Vec::new();
        for chunk in text.split("\n\n").filter(|c| !c.trim().is_empty()) {
            let mut event_type = String::new();
            let mut data = String::new();
            for line in chunk.lines() {
                if let Some(t) = line.strip_prefix("event: ") {
                    event_type = t.to_string();
                } else if let Some(d) = line.strip_prefix("data: ") {
                    data = d.to_string();
                } else {
                    panic!("malformed SSE line: {line:?}");
                }
            }
            let value: Value = serde_json::from_str(&data).expect("data is json");
            parsed.push((event_type, value));
        }
        (converter, parsed)
    }

    fn types(events: &[(String, Value)]) -> Vec<&str> {
        events.iter().map(|(t, _)| t.as_str()).collect()
    }

    fn find<'a>(events: &'a [(String, Value)], event_type: &str) -> &'a Value {
        events
            .iter()
            .find(|(t, _)| t == event_type)
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("no {event_type} in {:?}", types(events)))
    }

    /// The full `usage` object a terminal Responses event carries (every
    /// field the API documents, so nothing downstream reads an absent one).
    /// The output split is the live grok cap-16 measurement — 302 output
    /// tokens of which 286 are reasoning — because that is the shape where
    /// "which number is the output count" actually bites.
    fn usage_fixture() -> Value {
        json!({
            "input_tokens": 120,
            "input_tokens_details": {"cached_tokens": 64},
            "output_tokens": 302,
            "output_tokens_details": {"reasoning_tokens": 286},
            "total_tokens": 422,
        })
    }

    /// A complete `response.incomplete` envelope; `reason: None` omits
    /// `incomplete_details` entirely (the missing-discriminator case).
    fn incomplete_event(reason: Option<&str>) -> Value {
        let mut response = json!({
            "id": "resp_cap",
            "object": "response",
            "status": "incomplete",
            "model": "gpt-5.5",
            "output": [],
            "usage": usage_fixture(),
        });
        if let Some(reason) = reason {
            response["incomplete_details"] = json!({"reason": reason});
        }
        json!({
            "type": "response.incomplete",
            "sequence_number": 9,
            "response": response,
        })
    }

    #[test]
    fn response_incomplete_max_output_tokens_stops_cleanly() {
        let (converter, events) = run(&[
            json!({"type": "response.created",
                   "response": {"id": "resp_cap", "model": "gpt-5.5"}}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "delta": "half a sen"}),
            incomplete_event(Some("max_output_tokens")),
        ]);
        assert_eq!(
            types(&events),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ],
            "hitting the output cap is a clean end, never an `error` event"
        );
        assert_eq!(
            find(&events, "message_delta")["delta"]["stop_reason"],
            "max_tokens"
        );
        assert!(
            converter.error_message().is_none(),
            "a cap hit must not be scored as a provider failure"
        );
        let message = converter.into_message_json().expect("aggregate");
        assert_eq!(
            message["stop_reason"], "max_tokens",
            "the aggregate reports the same reason as the stream"
        );
        assert_eq!(message["content"][0]["text"], "half a sen");
    }

    #[test]
    fn incomplete_max_tokens_overrides_tool_use_stop_reason() {
        let (converter, events) = run(&[
            json!({"type": "response.created", "response": {"id": "resp_cap"}}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "function_call", "call_id": "c1", "name": "save",
                            "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "delta": "{\"v\":42}"}),
            json!({"type": "response.output_item.done",
                   "item": {"type": "function_call", "call_id": "c1", "name": "save",
                            "arguments": "{\"v\":42}"}}),
            incomplete_event(Some("max_output_tokens")),
        ]);
        assert_eq!(
            find(&events, "message_delta")["delta"]["stop_reason"],
            "max_tokens",
            "the cap outranks saw_tool_use"
        );
        let message = converter.into_message_json().expect("aggregate");
        assert_eq!(message["stop_reason"], "max_tokens");
        assert_eq!(
            message["content"][0]["input"]["v"], 42,
            "a tool call that DID finish before the cap still aggregates"
        );
    }

    #[test]
    fn incomplete_retains_upstream_usage() {
        let (converter, events) = run(&[
            json!({"type": "response.created", "response": {"id": "resp_cap"}}),
            incomplete_event(Some("max_output_tokens")),
        ]);
        let usage = &find(&events, "message_delta")["usage"];
        // input_tokens is the FRESH count: 120 cache-inclusive − 64 cached.
        assert_eq!(usage["input_tokens"], 56);
        assert_eq!(usage["cache_read_input_tokens"], 64);
        // `output_tokens` is already the SUM (302 = 16 visible + 286
        // reasoning on the live cap-16 capture); the reasoning split is
        // reporting detail, never something to subtract.
        assert_eq!(usage["output_tokens"], 302);
        assert_eq!(
            converter.usage(),
            StreamUsage {
                input_tokens: 56,
                output_tokens: 302,
                cache_read_input_tokens: Some(64),
                cache_creation_input_tokens: None,
            },
            "a capped turn still bills the tokens it really burned"
        );
        assert_eq!(
            converter.raw_usage(),
            Some(&usage_fixture()),
            "the trace keeps the verbatim splits on the capped path too"
        );
    }

    #[test]
    fn completed_event_with_incomplete_status_uses_incomplete_contract() {
        // Some backends deliver the terminal envelope as `response.completed`
        // while the response itself says `status: "incomplete"`. The status is
        // the truth.
        let mut completed = incomplete_event(Some("max_output_tokens"));
        completed["type"] = json!("response.completed");
        let (converter, events) = run(&[
            json!({"type": "response.created", "response": {"id": "resp_cap"}}),
            completed,
        ]);
        assert_eq!(
            find(&events, "message_delta")["delta"]["stop_reason"],
            "max_tokens",
            "`status: incomplete` outranks the `response.completed` envelope"
        );
        assert!(converter.into_message_json().is_some());
    }

    #[test]
    fn unknown_incomplete_reason_is_an_error_not_a_success() {
        let (converter, events) = run(&[
            json!({"type": "response.created", "response": {"id": "resp_cf"}}),
            incomplete_event(Some("content_filter")),
        ]);
        assert_eq!(types(&events), vec!["message_start", "error"]);
        assert_eq!(events[1].1["error"]["type"], "api_error");
        let message = converter
            .error_message()
            .expect("error message")
            .to_string();
        assert_eq!(events[1].1["error"]["message"], message);
        assert!(
            message.contains("content_filter"),
            "the operator must be able to read WHY from the error: {message:?}"
        );
        assert!(
            converter.into_message_json().is_none(),
            "a stop llmux cannot faithfully translate must not aggregate into a 200"
        );
    }

    #[test]
    fn incomplete_without_a_reason_is_an_error() {
        let (converter, events) = run(&[
            json!({"type": "response.created", "response": {"id": "resp_x"}}),
            incomplete_event(None),
        ]);
        assert_eq!(types(&events), vec!["message_start", "error"]);
        let message = converter
            .error_message()
            .expect("error message")
            .to_string();
        assert!(
            message.contains("incomplete") && message.contains("reason"),
            "a missing discriminator must say so, not borrow the truncation \
             message: {message:?}"
        );
        assert!(converter.into_message_json().is_none());
    }

    #[test]
    fn ordinary_completed_keeps_end_turn_and_tool_use() {
        // Regression: the widened stop-reason path must not touch normal
        // turns. `incomplete_details: null` rides on every successful
        // Responses payload, so the incomplete guard must key on `status`.
        let completed = json!({"type": "response.completed",
               "response": {"id": "r", "status": "completed",
                            "incomplete_details": null, "usage": usage_fixture()}});
        let (_, text_events) = run(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "delta": "hi"}),
            completed.clone(),
        ]);
        assert_eq!(
            find(&text_events, "message_delta")["delta"]["stop_reason"],
            "end_turn"
        );

        let (_, tool_events) = run(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "function_call", "call_id": "c1", "name": "save",
                            "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "delta": "{\"v\":1}"}),
            json!({"type": "response.output_item.done",
                   "item": {"type": "function_call", "call_id": "c1", "name": "save",
                            "arguments": "{\"v\":1}"}}),
            completed,
        ]);
        assert_eq!(
            find(&tool_events, "message_delta")["delta"]["stop_reason"],
            "tool_use"
        );
    }

    #[test]
    fn truncated_tool_arguments_never_become_an_executable_empty_input() {
        // The cap lands mid-argument JSON. Parsing `{"command":"rm -rf /tm`
        // fails, and the old fallback turned it into `{}` — a syntactically
        // valid, RUNNABLE tool call with its arguments silently erased.
        let (converter, _) = run(&[
            json!({"type": "response.created", "response": {"id": "resp_cut"}}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "delta": "removing it now"}),
            json!({"type": "response.output_item.done", "item": {"type": "message"}}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "function_call", "call_id": "c9", "name": "Bash",
                            "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta",
                   "delta": "{\"command\":\"rm -rf /tm"}),
            incomplete_event(Some("max_output_tokens")),
        ]);
        let message = converter.into_message_json().expect("aggregate");
        assert_eq!(message["stop_reason"], "max_tokens");
        let content = message["content"].as_array().expect("content array");
        assert!(
            !content.iter().any(|b| b["type"] == "tool_use"),
            "a half-written tool call must never surface as a runnable call: {content:?}"
        );
        assert_eq!(content.len(), 1, "the text the model did produce survives");
        assert_eq!(content[0]["text"], "removing it now");
    }

    #[test]
    fn argumentless_tool_call_still_aggregates_to_empty_input() {
        // Guard against over-correcting the rule above: a call whose upstream
        // `arguments` really is empty is a legitimate no-arg invocation.
        let (converter, _) = run(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "function_call", "call_id": "c0", "name": "now",
                            "arguments": ""}}),
            json!({"type": "response.output_item.done",
                   "item": {"type": "function_call", "call_id": "c0", "name": "now",
                            "arguments": ""}}),
            json!({"type": "response.completed",
                   "response": {"id": "r", "status": "completed", "usage": usage_fixture()}}),
        ]);
        let message = converter.into_message_json().expect("aggregate");
        assert_eq!(message["content"][0]["type"], "tool_use");
        assert_eq!(message["content"][0]["name"], "now");
        assert_eq!(
            message["content"][0]["input"],
            json!({}),
            "a genuinely argument-less call keeps its empty input"
        );
    }

    // ---- T3: output identity (interleaved / out-of-order items) ----

    /// Everything a client accumulates for the tool block at `index` before
    /// parsing its arguments.
    fn partial_json(events: &[(String, Value)], index: u64) -> String {
        events
            .iter()
            .filter(|(t, v)| {
                t == "content_block_delta"
                    && v["index"] == index
                    && v["delta"]["type"] == "input_json_delta"
            })
            .filter_map(|(_, v)| v["delta"]["partial_json"].as_str())
            .collect()
    }

    /// Every streamed text payload, concatenated in emission order — what the
    /// client renders across however many text blocks the wire used.
    fn streamed_text(events: &[(String, Value)]) -> String {
        events
            .iter()
            .filter(|(t, v)| t == "content_block_delta" && v["delta"]["type"] == "text_delta")
            .filter_map(|(_, v)| v["delta"]["text"].as_str())
            .collect()
    }

    /// A message item and a function_call item whose deltas INTERLEAVE, and
    /// whose `output_item.done` events arrive tool-first (non-sequential).
    /// Shapes match the documented Responses wire: deltas carry `item_id` +
    /// `output_index`, lifecycle events carry `output_index` + `item.id`.
    fn interleaved_stream() -> Vec<Value> {
        vec![
            json!({"type": "response.created",
                   "response": {"id": "resp_mix", "model": "gpt-5.5"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"id": "msg_1", "type": "message", "status": "in_progress",
                            "role": "assistant", "content": []}}),
            json!({"type": "response.output_text.delta", "item_id": "msg_1",
                   "output_index": 0, "content_index": 0, "delta": "I will "}),
            json!({"type": "response.output_item.added", "output_index": 1,
                   "item": {"id": "fc_1", "type": "function_call", "call_id": "call_1",
                            "name": "Bash", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "output_index": 1, "delta": "{\"command\":\"ls"}),
            // Text for item 0 arriving while item 1 is the live tool call.
            json!({"type": "response.output_text.delta", "item_id": "msg_1",
                   "output_index": 0, "content_index": 0, "delta": "check <tag> it."}),
            // The bytes the single-open-block converter silently discards.
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "output_index": 1, "delta": "\"}"}),
            json!({"type": "response.output_item.done", "output_index": 1,
                   "item": {"id": "fc_1", "type": "function_call", "call_id": "call_1",
                            "name": "Bash", "arguments": "{\"command\":\"ls\"}"}}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"id": "msg_1", "type": "message", "status": "completed",
                            "role": "assistant",
                            "content": [{"type": "output_text",
                                         "text": "I will check <tag> it."}]}}),
            json!({"type": "response.completed",
                   "response": {"id": "resp_mix", "status": "completed",
                                "incomplete_details": null, "usage": usage_fixture()}}),
        ]
    }

    #[test]
    fn interleaved_text_and_tool_items_keep_every_argument_byte() {
        let (converter, events) = run(&interleaved_stream());
        let message = converter.into_message_json().expect("aggregate");
        let tool = message["content"]
            .as_array()
            .expect("content")
            .iter()
            .find(|b| b["type"] == "tool_use")
            .expect("the tool call survives interleaving");
        assert_eq!(tool["name"], "Bash");
        assert_eq!(tool["id"], "call_1");
        assert_eq!(
            tool["input"]["command"], "ls",
            "argument deltas that arrive while another item is live must be \
             routed by item identity, not dropped"
        );
        // The same bytes must reach the streaming client, on ONE tool block.
        let tool_indexes: Vec<u64> = events
            .iter()
            .filter(|(t, v)| t == "content_block_start" && v["content_block"]["type"] == "tool_use")
            .filter_map(|(_, v)| v["index"].as_u64())
            .collect();
        assert_eq!(
            tool_indexes.len(),
            1,
            "one upstream call is one Anthropic block, never two half-written ones"
        );
        assert_eq!(
            partial_json(&events, tool_indexes[0]),
            "{\"command\":\"ls\"}"
        );
    }

    /// ONE message item carrying TWO content parts whose deltas interleave
    /// (A1 B1 A2 B2). Parts are the second axis of output identity: without
    /// `content_index` in the key every delta lands in one block and the
    /// client renders the scrambled `A1B1A2B2`.
    fn multipart_message_stream() -> Vec<Value> {
        vec![
            json!({"type": "response.created", "response": {"id": "resp_parts"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"id": "msg_1", "type": "message", "status": "in_progress",
                            "role": "assistant", "content": []}}),
            json!({"type": "response.output_text.delta", "item_id": "msg_1",
                   "output_index": 0, "content_index": 0, "delta": "A1"}),
            json!({"type": "response.output_text.delta", "item_id": "msg_1",
                   "output_index": 0, "content_index": 1, "delta": "B1"}),
            json!({"type": "response.output_text.delta", "item_id": "msg_1",
                   "output_index": 0, "content_index": 0, "delta": "A2"}),
            json!({"type": "response.output_text.delta", "item_id": "msg_1",
                   "output_index": 0, "content_index": 1, "delta": "B2"}),
            json!({"type": "response.content_part.done", "item_id": "msg_1",
                   "output_index": 0, "content_index": 0,
                   "part": {"type": "output_text", "annotations": [], "text": "A1A2"}}),
            json!({"type": "response.content_part.done", "item_id": "msg_1",
                   "output_index": 0, "content_index": 1,
                   "part": {"type": "output_text", "annotations": [], "text": "B1B2"}}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"id": "msg_1", "type": "message", "status": "completed",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "A1A2"},
                                        {"type": "output_text", "text": "B1B2"}]}}),
            json!({"type": "response.completed",
                   "response": {"id": "resp_parts", "status": "completed",
                                "incomplete_details": null, "usage": usage_fixture()}}),
        ]
    }

    #[test]
    fn interleaved_content_parts_stay_distinct_and_ordered() {
        let (converter, events) = run(&multipart_message_stream());
        let message = converter.into_message_json().expect("aggregate");
        let content = message["content"].as_array().expect("content");
        assert_eq!(
            content.len(),
            2,
            "two content parts of one item are two blocks, not one: {content:?}"
        );
        assert_eq!(
            content[0]["text"], "A1A2",
            "part 0 keeps only its own deltas, in order"
        );
        assert_eq!(content[1]["text"], "B1B2", "part 1 likewise");
        assert_eq!(
            events
                .iter()
                .filter(|(t, _)| t == "content_block_start")
                .count(),
            2,
            "no duplicate empty block from the item-level announcement: {events:?}"
        );
    }

    #[test]
    fn content_part_done_releases_the_wire_for_the_next_part() {
        // Part-level `done` must finish that part so a buffered sibling can
        // start streaming, rather than everything draining at the terminal.
        let (_, events) = run(&multipart_message_stream());
        let terminal = events
            .iter()
            .position(|(t, _)| t == "message_delta")
            .expect("message_delta");
        let closes = events
            .iter()
            .take(terminal)
            .filter(|(t, _)| t == "content_block_stop")
            .count();
        assert_eq!(
            closes, 2,
            "both parts close on their own `content_part.done`, before the tail"
        );
    }

    /// `(id, name)` of every `tool_use` block ANNOUNCED on the wire, in order.
    fn streamed_tool_calls(events: &[(String, Value)]) -> Vec<(String, String)> {
        events
            .iter()
            .filter(|(t, v)| t == "content_block_start" && v["content_block"]["type"] == "tool_use")
            .map(|(_, v)| {
                (
                    v["content_block"]["id"].as_str().unwrap_or("").to_string(),
                    v["content_block"]["name"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                )
            })
            .collect()
    }

    /// `(id, input)` of every `tool_use` block in the non-stream aggregate.
    fn aggregate_tool_calls(message: &Value) -> Vec<(String, Value)> {
        message["content"]
            .as_array()
            .expect("content")
            .iter()
            .filter(|b| b["type"] == "tool_use")
            .map(|b| {
                (
                    b["id"].as_str().unwrap_or("").to_string(),
                    b["input"].clone(),
                )
            })
            .collect()
    }

    #[test]
    fn a_buffered_argumentless_tool_call_still_reaches_the_stream() {
        // Two parallel calls: the first streams JSON and holds the wire, so
        // the second is buffered — and the second has NO arguments at all, so
        // it has zero bytes to flush. A "pending = has un-emitted bytes"
        // predicate skips it forever: it never reaches the wire while the
        // aggregate still lists it, telling the non-streaming client about a
        // call the streaming client never saw. The second also finishes FIRST,
        // so nothing about arrival order can rescue it.
        let (converter, events) = run(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"id": "fc_1", "type": "function_call", "call_id": "c1",
                            "name": "Bash", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "output_index": 0, "delta": "{\"x\":1}"}),
            json!({"type": "response.output_item.added", "output_index": 1,
                   "item": {"id": "fc_2", "type": "function_call", "call_id": "c2",
                            "name": "now", "arguments": ""}}),
            json!({"type": "response.output_item.done", "output_index": 1,
                   "item": {"id": "fc_2", "type": "function_call", "call_id": "c2",
                            "name": "now", "arguments": ""}}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"id": "fc_1", "type": "function_call", "call_id": "c1",
                            "name": "Bash", "arguments": "{\"x\":1}"}}),
            json!({"type": "response.completed",
                   "response": {"id": "r", "status": "completed",
                                "incomplete_details": null, "usage": usage_fixture()}}),
        ]);
        assert_eq!(
            streamed_tool_calls(&events),
            vec![
                ("c1".to_string(), "Bash".to_string()),
                ("c2".to_string(), "now".to_string()),
            ],
            "both calls are announced on the wire, once each, in item order"
        );
        let starts = events
            .iter()
            .filter(|(t, _)| t == "content_block_start")
            .count();
        let stops = events
            .iter()
            .filter(|(t, _)| t == "content_block_stop")
            .count();
        assert_eq!(starts, 2, "no duplicate blocks: {events:?}");
        assert_eq!(stops, starts, "every opened block closes exactly once");

        let message = converter.into_message_json().expect("aggregate");
        assert_eq!(
            aggregate_tool_calls(&message),
            vec![
                ("c1".to_string(), json!({"x": 1})),
                ("c2".to_string(), json!({})),
            ],
            "and the aggregate lists exactly the calls the stream announced"
        );
        assert_eq!(message["stop_reason"], "tool_use");
    }

    #[test]
    fn a_buffered_tool_call_interrupted_by_the_cap_is_inert_on_the_stream() {
        // Same buffering, but the cap fires before either call finishes. The
        // buffered call reaches the wire only via the terminal flush, so its
        // truncation marker must already be in its buffer — otherwise it is
        // emitted as a pristine, runnable `input: {}`.
        let (converter, events) = run(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"id": "fc_1", "type": "function_call", "call_id": "c1",
                            "name": "Bash", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "output_index": 0, "delta": "{\"command\":\"rm -rf /tm"}),
            json!({"type": "response.output_item.added", "output_index": 1,
                   "item": {"id": "fc_2", "type": "function_call", "call_id": "c2",
                            "name": "Write", "arguments": ""}}),
            incomplete_event(Some("max_output_tokens")),
        ]);
        let streamed = streamed_tool_calls(&events);
        assert_eq!(
            streamed.len(),
            2,
            "both calls reach the wire, so BOTH need a marker — the buffered \
             one arrives only via the terminal flush: {events:?}"
        );
        for (index, id) in streamed
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (i as u64, id.clone()))
        {
            let accumulated = partial_json(&events, index);
            assert!(
                serde_json::from_str::<Value>(&accumulated).is_err(),
                "interrupted call {id} must not accumulate into anything runnable, \
                 got {accumulated:?}"
            );
        }
        assert_eq!(
            find(&events, "message_delta")["delta"]["stop_reason"],
            "max_tokens"
        );
        let message = converter.into_message_json().expect("aggregate");
        assert!(
            aggregate_tool_calls(&message).is_empty(),
            "neither interrupted call is runnable, so neither is aggregated: {message:?}"
        );
    }

    #[test]
    fn interleaved_text_is_relayed_verbatim_and_whole() {
        let (converter, events) = run(&interleaved_stream());
        assert_eq!(
            streamed_text(&events),
            "I will check <tag> it.",
            "text buffered behind a live tool call is flushed, byte for byte — \
             no loss, no reordering within the item, no scrub of <tag>"
        );
        let message = converter.into_message_json().expect("aggregate");
        assert_eq!(
            message["content"][0]["type"], "text",
            "items keep upstream order in the aggregate"
        );
        assert_eq!(message["content"][0]["text"], "I will check <tag> it.");
    }

    #[test]
    fn anthropic_block_indexes_stay_dense_and_each_block_closes_once() {
        let (_, events) = run(&interleaved_stream());
        let mut open: Vec<u64> = Vec::new();
        let mut expected_next = 0u64;
        for (kind, value) in &events {
            match kind.as_str() {
                "content_block_start" => {
                    let index = value["index"].as_u64().expect("index");
                    assert_eq!(index, expected_next, "indexes must be dense: {events:?}");
                    expected_next += 1;
                    assert!(open.is_empty(), "only one block may be open: {events:?}");
                    open.push(index);
                }
                "content_block_delta" => {
                    let index = value["index"].as_u64().expect("index");
                    assert_eq!(
                        open.first(),
                        Some(&index),
                        "a delta may only target the open block: {events:?}"
                    );
                }
                "content_block_stop" => {
                    let index = value["index"].as_u64().expect("index");
                    assert_eq!(open.pop(), Some(index), "stop must match the open block");
                }
                "message_delta" => assert!(
                    open.is_empty(),
                    "every block closes before the terminal tail: {events:?}"
                ),
                _ => {}
            }
        }
        assert!(open.is_empty(), "a block was left open: {events:?}");
    }

    #[test]
    fn out_of_order_item_done_closes_the_right_block() {
        // `output_item.done` for the TEXT item arrives while the TOOL item is
        // the live block. Closing "whatever is open" would truncate the tool.
        let (converter, events) = run(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"id": "msg_1", "type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "item_id": "msg_1",
                   "output_index": 0, "delta": "a"}),
            json!({"type": "response.output_item.added", "output_index": 1,
                   "item": {"id": "fc_1", "type": "function_call", "call_id": "c1",
                            "name": "save", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "output_index": 1, "delta": "{\"x\":"}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"id": "msg_1", "type": "message", "role": "assistant"}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "output_index": 1, "delta": "1}"}),
            json!({"type": "response.output_item.done", "output_index": 1,
                   "item": {"id": "fc_1", "type": "function_call", "call_id": "c1",
                            "name": "save", "arguments": "{\"x\":1}"}}),
            json!({"type": "response.completed",
                   "response": {"id": "r", "status": "completed", "usage": usage_fixture()}}),
        ]);
        let message = converter.into_message_json().expect("aggregate");
        assert_eq!(
            message["content"][1]["input"]["x"], 1,
            "a done for a different item must not close the live tool block"
        );
        assert_eq!(message["content"][0]["text"], "a");
        assert_eq!(
            events
                .iter()
                .filter(|(t, _)| t == "content_block_start")
                .count(),
            2,
            "two upstream items, two blocks — no stray empty block: {events:?}"
        );
    }

    #[test]
    fn two_concurrent_tool_items_do_not_merge_arguments() {
        let (converter, _) = run(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"id": "fc_1", "type": "function_call", "call_id": "c1",
                            "name": "Bash", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "output_index": 0, "delta": "{\"command\":"}),
            json!({"type": "response.output_item.added", "output_index": 1,
                   "item": {"id": "fc_2", "type": "function_call", "call_id": "c2",
                            "name": "Read", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_2",
                   "output_index": 1, "delta": "{\"path\":\"/a\"}"}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "output_index": 0, "delta": "\"ls\"}"}),
            json!({"type": "response.output_item.done", "output_index": 1,
                   "item": {"id": "fc_2", "type": "function_call", "call_id": "c2",
                            "name": "Read", "arguments": "{\"path\":\"/a\"}"}}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"id": "fc_1", "type": "function_call", "call_id": "c1",
                            "name": "Bash", "arguments": "{\"command\":\"ls\"}"}}),
            json!({"type": "response.completed",
                   "response": {"id": "r", "status": "completed", "usage": usage_fixture()}}),
        ]);
        let message = converter.into_message_json().expect("aggregate");
        let content = message["content"].as_array().expect("content");
        assert_eq!(content.len(), 2, "two calls, two blocks: {content:?}");
        assert_eq!(content[0]["name"], "Bash");
        assert_eq!(
            content[0]["input"]["command"], "ls",
            "each call owns its own argument buffer"
        );
        assert_eq!(content[1]["name"], "Read");
        assert_eq!(content[1]["input"]["path"], "/a");
    }

    #[test]
    fn legacy_events_without_item_identity_keep_single_block_behavior() {
        // Pre-`output_index` captures (and every existing codex test) carry no
        // item identity at all. Identity routing must degrade to exactly the
        // old single-active-block behavior for them.
        let (converter, events) = run(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "delta": "checking"}),
            json!({"type": "response.output_item.done", "item": {"type": "message"}}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "function_call", "call_id": "call_9",
                            "name": "get_weather", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "delta": "{\"city\":"}),
            json!({"type": "response.function_call_arguments.delta", "delta": "\"Seoul\"}"}),
            json!({"type": "response.output_item.done",
                   "item": {"type": "function_call", "call_id": "call_9",
                            "name": "get_weather",
                            "arguments": "{\"city\":\"Seoul\"}"}}),
            json!({"type": "response.completed",
                   "response": {"id": "r", "status": "completed", "usage": usage_fixture()}}),
        ]);
        assert_eq!(
            types(&events),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[4].1["index"], 1, "indexes sequence 0, 1");
        assert_eq!(events[5].1["delta"]["partial_json"], "{\"city\":");
        assert_eq!(events[6].1["delta"]["partial_json"], "\"Seoul\"}");
        let message = converter.into_message_json().expect("aggregate");
        assert_eq!(message["content"][1]["input"]["city"], "Seoul");
    }

    // ---- §3b: tool arguments that are not an executable object ----

    /// A normal (uncapped) turn whose single function_call accumulates
    /// `args` — the shape that must be refused rather than silently dropped.
    fn completed_with_tool_args(args: &str) -> Vec<Value> {
        vec![
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"id": "fc_1", "type": "function_call", "call_id": "c1",
                            "name": "Bash", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "output_index": 0, "delta": args}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"id": "fc_1", "type": "function_call", "call_id": "c1",
                            "name": "Bash", "arguments": args}}),
            json!({"type": "response.completed",
                   "response": {"id": "r", "status": "completed",
                                "incomplete_details": null, "usage": usage_fixture()}}),
        ]
    }

    /// Assert that a normal completion carrying `args` fails the whole
    /// response instead of quietly shipping a tool-less `tool_use` turn.
    fn assert_protocol_error(args: &str) {
        let (converter, events) = run(&completed_with_tool_args(args));
        assert_eq!(
            types(&events).last(),
            Some(&"error"),
            "a completed turn whose tool arguments are not an object is a \
             protocol violation, not a clean end: args={args:?} {events:?}"
        );
        assert!(
            !types(&events).contains(&"message_stop"),
            "a violated response must not also look finished: args={args:?}"
        );
        let message = converter
            .error_message()
            .expect("error message")
            .to_string();
        assert!(
            message.contains("Bash"),
            "the error must name the offending tool: {message:?}"
        );
        assert!(
            converter.into_message_json().is_none(),
            "aggregate must be a 502, not a 200 with the tool silently missing"
        );
    }

    #[test]
    fn malformed_tool_arguments_on_normal_completion_are_a_protocol_error() {
        // Nothing truncated this turn — the upstream called it complete — so
        // dropping the block would ship a `stop_reason: tool_use` turn with
        // no tool in it: a lie about a finished response.
        assert_protocol_error("{\"command\":\"ls");
    }

    #[test]
    fn scalar_tool_arguments_on_normal_completion_are_a_protocol_error() {
        // Valid JSON, but an Anthropic `tool_use.input` must be an OBJECT.
        assert_protocol_error("42");
    }

    #[test]
    fn array_tool_arguments_on_normal_completion_are_a_protocol_error() {
        assert_protocol_error("[1,2]");
    }

    #[test]
    fn interrupted_toolless_arguments_are_not_executable_on_the_stream() {
        // The cap fires after the tool item opened but before ANY argument
        // delta. A client accumulating zero partial_json would materialize
        // `{}` and run it, so the converter must leave the accumulation
        // unparseable — on the stream as well as in the aggregate.
        let (converter, events) = run(&[
            json!({"type": "response.created", "response": {"id": "resp_cut"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"id": "msg_1", "type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "item_id": "msg_1",
                   "output_index": 0, "delta": "let me run"}),
            json!({"type": "response.output_item.added", "output_index": 1,
                   "item": {"id": "fc_1", "type": "function_call", "call_id": "c1",
                            "name": "Bash", "arguments": ""}}),
            incomplete_event(Some("max_output_tokens")),
        ]);
        let tool_index = events
            .iter()
            .find(|(t, v)| t == "content_block_start" && v["content_block"]["type"] == "tool_use")
            .and_then(|(_, v)| v["index"].as_u64())
            .expect("the tool block was already announced on the wire");
        let accumulated = partial_json(&events, tool_index);
        assert!(
            serde_json::from_str::<Value>(&accumulated).is_err(),
            "an interrupted tool's accumulated arguments must not parse into \
             anything runnable, got {accumulated:?}"
        );
        assert_eq!(
            find(&events, "message_delta")["delta"]["stop_reason"],
            "max_tokens",
            "the cap is still reported honestly"
        );
        let message = converter.into_message_json().expect("aggregate");
        let content = message["content"].as_array().expect("content");
        assert!(
            !content.iter().any(|b| b["type"] == "tool_use"),
            "and the aggregate drops it rather than shipping `{{}}`: {content:?}"
        );
        assert_eq!(
            content[0]["text"], "let me run",
            "partial text is preserved"
        );
    }
}
