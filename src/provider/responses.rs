//! Shared OpenAI-Responses-API machinery (R5 of `docs/grok/spec.md`): the
//! provider-agnostic Anthropic Messages ↔ Responses translation and the
//! Responses-SSE → Anthropic-SSE converter, extracted verbatim from the codex
//! provider (which pioneered the path). Per-provider adapters — codex, grok —
//! resolve their own model/effort/headers and hand this module a
//! [`RequestPlan`]; behavior knobs that differ between backends live as plan
//! flags, never as forks of the translation itself.

use serde_json::{json, Map, Value};

use super::ProviderError;
use crate::proxy::sse::{SseTransform, StreamUsage};

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

/// Translate an Anthropic Messages body into a Responses API body under
/// `plan`. Returns `(upstream_body, client_requested_stream)`; upstream is
/// always `stream: true` (non-stream clients get the aggregated result).
/// `max_tokens` and `tool_choice` are ignored (logged at debug); images and
/// thinking blocks are dropped (warn/debug).
pub fn build_responses_body(
    body: &Value,
    plan: &RequestPlan<'_>,
) -> Result<(Value, bool), ProviderError> {
    let client_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if body.get("tool_choice").is_some() {
        tracing::debug!("responses: tool_choice ignored");
    }

    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::Convert("request has no messages array".into()))?;
    // Responses endpoints reject any `role:"system"` input item ("System
    // messages are not allowed"). Anthropic top-level `system` and any
    // mid-conversation `messages[].role:"system"` (Claude Code's operator /
    // `<system-reminder>` channel) both fold into `instructions` — never into
    // an `input` item.
    let (input, folded_system) = messages_to_input(messages)?;
    let instructions = build_instructions(body.get("system"), &folded_system);
    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| tools_to_functions(tools))
        .unwrap_or_default();

    let mut upstream = json!({
        "model": plan.upstream_model,
        "instructions": instructions,
        "input": input,
        "tools": tools,
        "parallel_tool_calls": true,
        "store": false,
        "stream": true,
        "prompt_cache_key": plan.session_id,
    });
    if plan.include_encrypted_reasoning {
        upstream["include"] = json!(["reasoning.encrypted_content"]);
    }
    // Reasoning effort: omit to keep the backend default (`None` plan value).
    if let Some(effort) = plan.effort.as_deref() {
        upstream["reasoning"] = json!({ "effort": effort });
    }
    // Fast mode: codex stores "fast" in config but sends `service_tier:
    // "priority"` on the wire. Only emit the field when the plan asks.
    if plan.priority_tier {
        upstream["service_tier"] = json!("priority");
    }
    Ok((upstream, client_stream))
}

fn system_text(system: &Value) -> String {
    match system {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Compose the Responses `instructions` string from the Anthropic top-level
/// `system` field plus any system-role messages folded out of `messages[]`.
/// Both are operator instructions, so they concatenate in order (top-level
/// first, then mid-conversation ones as they appeared). This is the only place
/// system content goes — it is never emitted as an `input` item, since codex
/// rejects `role:"system"` items.
fn build_instructions(system: Option<&Value>, folded_system: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(text) = system.map(system_text).filter(|s| !s.is_empty()) {
        parts.push(text);
    }
    parts.extend(folded_system.iter().filter(|s| !s.is_empty()).cloned());
    parts.join("\n")
}

/// The role to stamp on a codex `input` message item. Codex's `responses`
/// endpoint accepts `user`, `assistant`, and `developer`, and rejects
/// `system` ("System messages are not allowed", verified live). Assistant
/// turns map to `assistant`; everything else maps to `user` (system-role
/// messages never reach here — they are folded into `instructions`).
fn input_role(anthropic_role: &str) -> &'static str {
    match anthropic_role {
        "assistant" => "assistant",
        "developer" => "developer",
        _ => "user",
    }
}

/// Anthropic `messages[]` → Responses `input[]` items, plus the text of any
/// `role:"system"` messages (returned separately to fold into `instructions`).
fn messages_to_input(messages: &[Value]) -> Result<(Vec<Value>, Vec<String>), ProviderError> {
    let mut input: Vec<Value> = Vec::new();
    let mut folded_system: Vec<String> = Vec::new();
    for message in messages {
        let anthropic_role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        // System-role messages (Claude Code's mid-conversation operator channel)
        // cannot become input items — codex 400s on `role:"system"`. Pull their
        // text out for `instructions` and emit no input item.
        if anthropic_role == "system" {
            let text = message_text(message);
            if !text.is_empty() {
                folded_system.push(text);
            }
            continue;
        }
        let role = input_role(anthropic_role);
        let text_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        let flush_text = |input: &mut Vec<Value>, text: &mut String| {
            if !text.is_empty() {
                input.push(json!({
                    "type": "message",
                    "role": role,
                    "content": [{"type": text_type, "text": std::mem::take(text)}],
                }));
            }
        };
        match message.get("content") {
            Some(Value::String(text)) => {
                let mut text = text.clone();
                flush_text(&mut input, &mut text);
            }
            Some(Value::Array(blocks)) => {
                let mut text = String::new();
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = block.get("text").and_then(Value::as_str) {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(t);
                            }
                        }
                        Some("tool_use") => {
                            flush_text(&mut input, &mut text);
                            let arguments = block
                                .get("input")
                                .map(|i| i.to_string())
                                .unwrap_or_else(|| "{}".to_string());
                            input.push(json!({
                                "type": "function_call",
                                "call_id": block.get("id").and_then(Value::as_str).unwrap_or(""),
                                "name": block.get("name").and_then(Value::as_str).unwrap_or(""),
                                "arguments": arguments,
                            }));
                        }
                        Some("tool_result") => {
                            flush_text(&mut input, &mut text);
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": block
                                    .get("tool_use_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or(""),
                                "output": tool_result_text(block),
                            }));
                        }
                        Some("image") => {
                            tracing::warn!(
                                "responses: image content block dropped (unsupported in v1)"
                            );
                        }
                        // Thinking blocks from a previous codex turn cannot be
                        // replayed upstream — drop them.
                        Some("thinking") | Some("redacted_thinking") => {
                            tracing::debug!("responses: thinking block dropped on request side");
                        }
                        other => {
                            tracing::debug!(block_type = ?other, "responses: unknown content block dropped");
                        }
                    }
                }
                flush_text(&mut input, &mut text);
            }
            _ => {}
        }
    }
    Ok((input, folded_system))
}

/// Plain text of an Anthropic message's `content` (string, or the `text`
/// blocks of a content array joined by newlines). Used to fold a system-role
/// message into `instructions`.
fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `tool_result.content` (string or text-block array) → plain text output.
fn tool_result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Anthropic `tools[]` → Responses function tools. Entries without a name
/// (e.g. server-side tool types) are dropped with a debug log.
fn tools_to_functions(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?;
            let mut function = Map::new();
            function.insert("type".into(), json!("function"));
            function.insert("name".into(), json!(name));
            if let Some(description) = tool.get("description").and_then(Value::as_str) {
                function.insert("description".into(), json!(description));
            }
            function.insert(
                "parameters".into(),
                tool.get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            );
            function.insert("strict".into(), json!(false));
            Some(Value::Object(function))
        })
        .collect()
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

/// chars/4 token estimate for one request section (e.g. just `system`, just
/// `tools`, or just `messages`) so the trace can report the input breakdown
/// per part. NOT floored — sum the parts, then floor the total if needed.
pub fn estimate_section_tokens(value: &Value) -> u64 {
    section_chars(value) / 4
}

/// Naive input-token estimate for `/v1/messages/count_tokens` on a codex
/// account (no upstream equivalent): total characters of system + message
/// text, divided by 4, floor 1.
pub fn estimate_input_tokens(body: &Value) -> u64 {
    let mut total = 0u64;
    if let Some(system) = body.get("system") {
        total += section_chars(system);
    }
    if let Some(messages) = body.get("messages") {
        total += section_chars(messages);
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

/// One finished/open content block, kept for the non-stream aggregate.
#[derive(Debug)]
struct AggBlock {
    kind: BlockKind,
    text: String,
    tool_id: String,
    tool_name: String,
    tool_args: String,
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
    /// Index of the currently open content block, if any (its kind is the
    /// kind of the LAST entry in `blocks`).
    open_index: Option<usize>,
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
            open_index: None,
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

    fn open_block(&mut self, out: &mut Vec<u8>, kind: BlockKind, content_block: Value) -> usize {
        self.close_block(out);
        let index = self.next_index;
        self.next_index += 1;
        self.open_index = Some(index);
        self.blocks.push(AggBlock {
            kind,
            text: String::new(),
            tool_id: String::new(),
            tool_name: String::new(),
            tool_args: String::new(),
        });
        Self::emit(
            out,
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": index,
                "content_block": content_block,
            }),
        );
        index
    }

    fn close_block(&mut self, out: &mut Vec<u8>) {
        if let Some(index) = self.open_index.take() {
            Self::emit(
                out,
                "content_block_stop",
                &json!({"type": "content_block_stop", "index": index}),
            );
        }
    }

    fn open_kind(&self) -> Option<BlockKind> {
        self.open_index.map(|_| {
            self.blocks
                .last()
                .map(|b| b.kind)
                .unwrap_or(BlockKind::Text)
        })
    }

    fn ensure_block(&mut self, out: &mut Vec<u8>, kind: BlockKind) -> usize {
        if self.open_kind() == Some(kind) {
            return self.open_index.unwrap_or(0);
        }
        let content_block = match kind {
            BlockKind::Text => json!({"type": "text", "text": ""}),
            BlockKind::Thinking => json!({"type": "thinking", "thinking": ""}),
            // Tool blocks are only ever opened explicitly with id+name.
            BlockKind::ToolUse => json!({"type": "tool_use", "id": "", "name": "", "input": {}}),
        };
        self.open_block(out, kind, content_block)
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

    fn complete(&mut self, out: &mut Vec<u8>, response: Option<&Value>) {
        if self.finished {
            return;
        }
        self.ensure_started(out);
        self.close_block(out);
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
        let stop_reason = if self.saw_tool_use {
            "tool_use"
        } else {
            "end_turn"
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

    /// Track aggregate content for the non-stream response.
    fn aggregate(&mut self, kind: BlockKind, push: impl FnOnce(&mut AggBlock)) {
        if let Some(block) = self.blocks.last_mut() {
            if block.kind == kind {
                push(block);
            }
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
            .map(|block| match block.kind {
                BlockKind::Text => json!({"type": "text", "text": block.text}),
                BlockKind::Thinking => json!({"type": "thinking", "thinking": block.text}),
                BlockKind::ToolUse => json!({
                    "type": "tool_use",
                    "id": block.tool_id,
                    "name": block.tool_name,
                    "input": serde_json::from_str::<Value>(&block.tool_args)
                        .unwrap_or_else(|_| json!({})),
                }),
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
                let item = value.get("item");
                match item.and_then(|i| i.get("type")).and_then(Value::as_str) {
                    Some("message") => {
                        self.ensure_block(&mut out, BlockKind::Text);
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
                        self.open_block(
                            &mut out,
                            BlockKind::ToolUse,
                            json!({"type": "tool_use", "id": call_id, "name": name, "input": {}}),
                        );
                        if let Some(block) = self.blocks.last_mut() {
                            block.tool_id = call_id;
                            block.tool_name = name;
                        }
                    }
                    _ => {} // reasoning items etc. open lazily via their deltas
                }
            }
            "response.output_text.delta" => {
                self.ensure_started(&mut out);
                let index = self.ensure_block(&mut out, BlockKind::Text);
                let text = value
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.delta(&mut out, index, json!({"type": "text_delta", "text": text}));
                self.aggregate(BlockKind::Text, |b| b.text.push_str(&text));
            }
            "response.reasoning_summary_text.delta" => {
                self.ensure_started(&mut out);
                let index = self.ensure_block(&mut out, BlockKind::Thinking);
                let text = value
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.delta(
                    &mut out,
                    index,
                    json!({"type": "thinking_delta", "thinking": text}),
                );
                self.aggregate(BlockKind::Thinking, |b| b.text.push_str(&text));
            }
            "response.function_call_arguments.delta" => {
                if self.open_kind() == Some(BlockKind::ToolUse) {
                    let index = self.open_index.unwrap_or(0);
                    let partial = value
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    self.delta(
                        &mut out,
                        index,
                        json!({"type": "input_json_delta", "partial_json": partial}),
                    );
                    self.aggregate(BlockKind::ToolUse, |b| b.tool_args.push_str(&partial));
                }
            }
            "response.output_item.done" => {
                // A function_call item may deliver its full arguments only
                // here (no deltas streamed) — emit them before closing.
                if self.open_kind() == Some(BlockKind::ToolUse) {
                    let streamed = self
                        .blocks
                        .last()
                        .map(|b| !b.tool_args.is_empty())
                        .unwrap_or(false);
                    if !streamed {
                        if let Some(arguments) = value
                            .get("item")
                            .and_then(|i| i.get("arguments"))
                            .and_then(Value::as_str)
                            .filter(|a| !a.is_empty())
                        {
                            let index = self.open_index.unwrap_or(0);
                            let arguments = arguments.to_string();
                            self.delta(
                                &mut out,
                                index,
                                json!({"type": "input_json_delta", "partial_json": arguments}),
                            );
                            self.aggregate(BlockKind::ToolUse, |b| {
                                b.tool_args.push_str(&arguments)
                            });
                        }
                    }
                }
                self.close_block(&mut out);
            }
            "response.completed" => {
                self.complete(&mut out, value.get("response"));
            }
            "response.failed" => {
                let message = value
                    .get("response")
                    .and_then(|r| r.get("error"))
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("upstream response failed")
                    .to_string();
                self.fail(&mut out, &message);
            }
            "error" => {
                let message = value
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        value
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("upstream error")
                    .to_string();
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
