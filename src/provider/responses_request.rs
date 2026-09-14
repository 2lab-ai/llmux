//! Anthropic Messages → OpenAI-Responses **request** translation with an
//! explicit compatibility contract (`docs/responses-compatibility/spec.md`).
//!
//! The predecessor translator (`super::responses::build_responses_body`)
//! silently discarded whatever it could not express: image blocks, nested
//! tool-result images, `tool_choice`, `max_tokens`, unknown content blocks and
//! nameless tools all vanished with a log line the client never sees. This
//! module replaces that policy with two rules:
//!
//! 1. **Convert what the endpoints actually accept.** Base64 PNG/JPEG images
//!    (top level and nested in a `tool_result`), every `tool_choice` variant,
//!    and — on grok — `max_tokens`. All four are live-verified against the two
//!    OAuth backends llmux speaks to (receipts in the spec's §Evidence).
//! 2. **Never drop the rest silently.** Anything unsupported is a typed
//!    [`ProviderError::InvalidRequest`] naming the JSON path (HTTP 400 locally,
//!    no upstream call, no credential refresh). The only tolerated losses are
//!    the two the wire makes unavoidable — a codex `max_tokens` and prior
//!    `thinking` state — and those are reported in a
//!    [`CompatibilityReport`] the proxy turns into response headers.
//!
//! Error strings carry the field PATH (`messages[2].content[1].source.data`)
//! and never the payload itself: a 20 MiB base64 blob must not land in a log.

use base64::Engine;
use serde_json::{json, Map, Value};

use super::responses::RequestPlan;
use super::ProviderError;

/// Largest decoded image llmux forwards, per block. Beyond this the request is
/// rejected locally rather than burning an upstream round trip on a payload
/// both backends refuse.
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// Media types accepted for a base64 image. Anthropic also allows GIF/WebP;
/// neither backend's documented input-image contract covers them, so they are
/// rejected rather than converted into a data URL that may 400 upstream.
const SUPPORTED_IMAGE_MEDIA_TYPES: &[&str] = &["image/png", "image/jpeg"];

/// Leading marker for a `tool_result` whose `is_error` is true. Responses has
/// no error flag on `function_call_output`, and presenting a failed tool result
/// as a successful one is a correctness bug (the model reports success it never
/// got), so the failure rides in the text — namespaced so it cannot be confused
/// with tool output.
const TOOL_RESULT_ERROR_MARKER: &str = "[llmux:tool-result-error]";

/// Which Responses backend the body is being built for. The translation is
/// shared; only the few places where the two endpoints provably differ branch
/// on this (see the spec's compatibility matrix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsesFlavor {
    /// `chatgpt.com/backend-api/codex/responses` (ChatGPT subscription).
    Codex,
    /// `cli-chat-proxy.grok.com/v1/responses` (Grok subscription).
    Grok,
}

/// What this request loses on the way upstream. Empty = fully faithful.
///
/// `omitted_fields` names the request fields that are NOT forwarded;
/// `warnings` is the superset the proxy reports: every omitted field plus
/// semantic caveats that omit nothing (today only `max_tokens_semantics`).
/// Both are sorted and deduped so the header value is stable across requests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompatibilityReport {
    pub omitted_fields: Vec<&'static str>,
    pub warnings: Vec<&'static str>,
}

impl CompatibilityReport {
    /// No loss and no caveat — the request goes upstream faithfully.
    pub fn is_empty(&self) -> bool {
        self.omitted_fields.is_empty() && self.warnings.is_empty()
    }

    /// Record a field that is dropped: it is both an omission and a warning.
    fn omit(&mut self, field: &'static str) {
        self.omitted_fields.push(field);
        self.warnings.push(field);
    }

    /// Record a caveat that omits nothing (the value IS forwarded, its meaning
    /// upstream is not provably identical).
    fn warn(&mut self, warning: &'static str) {
        self.warnings.push(warning);
    }

    fn finish(mut self) -> Self {
        self.omitted_fields.sort_unstable();
        self.omitted_fields.dedup();
        self.warnings.sort_unstable();
        self.warnings.dedup();
        self
    }
}

/// Validate an Anthropic Messages body against what `flavor` can express and
/// report the residual loss. Returns `Err` for anything that would otherwise be
/// silently dropped — the proxy answers 400 with the message, before any
/// upstream call or credential refresh.
///
/// `count_tokens` switches to the `/v1/messages/count_tokens` contract: images
/// are rejected (no honest token estimate exists for them) and the
/// inference-only `max_tokens` caveats are skipped, since counting sends no
/// output budget anywhere.
///
/// Runs the SAME conversion [`build_responses_body`] runs, so a body that
/// validates here cannot fail there and vice versa.
pub fn validate_request(
    body: &Value,
    flavor: ResponsesFlavor,
    count_tokens: bool,
) -> Result<CompatibilityReport, ProviderError> {
    Ok(convert(body, flavor, count_tokens)?.report)
}

/// Translate an Anthropic Messages body into a Responses API body under
/// `plan` + `flavor`. Returns `(upstream_body, client_requested_stream)`;
/// upstream is always `stream: true` (non-stream clients get the aggregated
/// result).
///
/// Validation is not optional: this calls [`validate_request`]'s conversion
/// itself, so a caller that skips validation still cannot ship a body with
/// silently-dropped content.
pub fn build_responses_body(
    body: &Value,
    plan: &RequestPlan<'_>,
    flavor: ResponsesFlavor,
) -> Result<(Value, bool), ProviderError> {
    let client_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let converted = convert(body, flavor, false)?;

    let mut upstream = json!({
        "model": plan.upstream_model,
        "instructions": converted.instructions,
        "input": converted.input,
        "store": false,
        "stream": true,
    });
    // Tools travel as a trio. With no tools, `tool_choice` is vacuous
    // (`auto`/`none` mean the same thing) and `parallel_tool_calls` has nothing
    // to parallelize, while xAI is documented to REJECT a `tool_choice` without
    // tools — so all three are omitted together on both flavors.
    if let Some(tools) = converted.tools {
        upstream["tools"] = json!(tools);
        upstream["parallel_tool_calls"] = json!(converted.parallel_tool_calls.unwrap_or(true));
        if let Some(choice) = converted.tool_choice {
            upstream["tool_choice"] = choice;
        }
    }
    // `max_output_tokens` is only ever set for grok (codex 400s on it); the
    // flavor decision happened in `convert`.
    if let Some(max_output_tokens) = converted.max_output_tokens {
        upstream["max_output_tokens"] = json!(max_output_tokens);
    }
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
    // `prompt_cache_key` is a codex cache hint. It is a PROCESS-wide id, so on
    // grok — whose cli-chat-proxy routing scope for the key is undocumented —
    // it would assert a session grouping llmux cannot back with evidence.
    if flavor == ResponsesFlavor::Codex {
        upstream["prompt_cache_key"] = json!(plan.session_id);
    }
    Ok((upstream, client_stream))
}

/// Everything the conversion produces: the upstream body parts plus the
/// residual-loss report. Kept separate from the JSON assembly so
/// [`validate_request`] can run the identical checks without building a body.
struct Converted {
    input: Vec<Value>,
    instructions: String,
    /// `None` = emit no `tools`/`tool_choice`/`parallel_tool_calls` at all.
    tools: Option<Vec<Value>>,
    tool_choice: Option<Value>,
    parallel_tool_calls: Option<bool>,
    max_output_tokens: Option<u64>,
    report: CompatibilityReport,
}

fn invalid(path: &str, message: &str) -> ProviderError {
    ProviderError::InvalidRequest(format!("{path}: {message}"))
}

fn convert(
    body: &Value,
    flavor: ResponsesFlavor,
    count_tokens: bool,
) -> Result<Converted, ProviderError> {
    if !body.is_object() {
        return Err(invalid("request body", "expected a JSON object"));
    }
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("messages", "expected an array"))?;

    let mut report = CompatibilityReport::default();
    let mut input: Vec<Value> = Vec::new();
    // The codex endpoint rejects a `role:"system"` input item ("System
    // messages are not allowed", verified live); grok has no such receipt —
    // folding there is llmux's own bridge policy, kept uniform because
    // `instructions` is the one channel both endpoints document for operator
    // text. Anthropic top-level `system` and any mid-conversation
    // `messages[].role:"system"` (Claude Code's operator / `<system-reminder>`
    // channel) therefore both fold into `instructions`.
    let mut folded_system: Vec<String> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        convert_message(
            index,
            message,
            count_tokens,
            &mut input,
            &mut folded_system,
            &mut report,
        )?;
    }
    let instructions = build_instructions(body.get("system"), &folded_system)?;

    let tools = match body.get("tools") {
        None | Some(Value::Null) => Vec::new(),
        Some(value) => convert_tools(value)?,
    };
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    let mut tool_choice = None;
    let mut parallel_tool_calls = None;
    if let Some(choice) = body.get("tool_choice").filter(|v| !v.is_null()) {
        let (choice, parallel) = convert_tool_choice(choice, &names)?;
        tool_choice = choice;
        parallel_tool_calls = parallel;
    }
    let tools = if tools.is_empty() {
        // Validation above still ran (a named choice with no tools is an
        // error, not a silent drop); what is dropped here is only vacuous.
        tool_choice = None;
        parallel_tool_calls = None;
        None
    } else {
        Some(tools)
    };

    let mut max_output_tokens = None;
    // ABSENT is legal (the no-cap body the codex idle probe and count_tokens
    // send); anything PRESENT — `null` included — must be a positive integer,
    // so an explicit limit is never quietly treated as "no limit asked".
    if let Some(value) = body.get("max_tokens") {
        let max = value
            .as_u64()
            .filter(|max| *max > 0)
            .ok_or_else(|| invalid("max_tokens", "expected a positive integer"))?;
        // Counting sends no output budget anywhere, so neither the omission
        // nor the semantics caveat applies to a count request.
        if !count_tokens {
            match flavor {
                // Live receipt: the codex OAuth backend answers
                // `400 Unsupported parameter: max_output_tokens` (probes
                // 2026-09-11 and 2026-09-14, cap 16 and cap 1). A search for a
                // substitute found no supported alternative cap parameter for
                // this backend — an absence of evidence over the fields looked
                // at, NOT proof that none exists; if one is ever verified, the
                // cap becomes forwardable here. Until then the cap is omitted
                // and REPORTED: llmux will neither send a field the backend
                // refuses nor fake enforcement by truncating locally.
                ResponsesFlavor::Codex => report.omit("max_tokens"),
                // Grok accepts the cap (live receipt: cap 16 → `incomplete` /
                // `max_output_tokens`). It is NOT provably the same budget:
                // the same capture REPORTED 302 output tokens, 286 of them
                // reasoning, so what the cap bounds is not the number the
                // client thinks it bounded. (What is billed for those tokens
                // is a separate question this receipt does not answer.)
                ResponsesFlavor::Grok => {
                    max_output_tokens = Some(max);
                    report.warn("max_tokens_semantics");
                }
            }
        }
    }

    // Generation controls outside the verified mapping. The previous
    // translator read NONE of these: the turn ran at the backend default while
    // the client believed it had set a temperature or a stop sequence. Neither
    // subscription endpoint's acceptance is verified (the public vendor APIs
    // are not these endpoints), so llmux refuses rather than forwarding on a
    // public-API assumption — and refuses rather than pretending.
    for field in ["temperature", "top_p", "top_k"] {
        if body.get(field).is_some_and(|value| !value.is_null()) {
            return Err(invalid(
                field,
                "sampling controls are not forwarded to the Responses backends \
                 (acceptance unverified); omit it to use the backend default",
            ));
        }
    }
    match body.get("stop_sequences") {
        // Absent, null, or an EMPTY array asks for nothing.
        None | Some(Value::Null) => {}
        Some(Value::Array(sequences)) if sequences.is_empty() => {}
        Some(Value::Array(_)) => {
            return Err(invalid(
                "stop_sequences",
                "stop sequences are not forwarded to the Responses backends \
                 (acceptance unverified); llmux will not emulate them locally",
            ))
        }
        Some(_) => return Err(invalid("stop_sequences", "expected an array of strings")),
    }
    // Top-level `thinking` is a CONFIG, not the private history blocks handled
    // per message — hence its own report name. It is validated and reported,
    // never translated: mapping a `budget_tokens` onto a reasoning effort would
    // invent a correspondence neither backend documents, and `disabled` cannot
    // be honored at all (grok-4.6 documents that reasoning cannot be turned
    // off). The proxy's activity log reads it for a label; the wire never does.
    if let Some(thinking) = body.get("thinking").filter(|value| !value.is_null()) {
        validate_thinking_config(thinking)?;
        // Counting sends no generation control upstream, so nothing is lost
        // there — but a malformed config is still worth telling the client.
        if !count_tokens {
            report.omit("thinking_config");
        }
    }

    Ok(Converted {
        input,
        instructions,
        tools,
        tool_choice,
        parallel_tool_calls,
        max_output_tokens,
        report: report.finish(),
    })
}

/// Shape check for the top-level `thinking` config. Nothing here reaches the
/// wire — the point is that a client which MISSPELLED its extended-thinking
/// request hears about it, instead of having a silently ignored field reported
/// as a mere omission.
fn validate_thinking_config(thinking: &Value) -> Result<(), ProviderError> {
    if !thinking.is_object() {
        return Err(invalid("thinking", "expected an object"));
    }
    let kind = thinking
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("thinking.type", "expected a string"))?;
    match kind {
        "enabled" => {
            // Validated, never mapped: no budget is forwarded or enforced.
            thinking
                .get("budget_tokens")
                .and_then(Value::as_u64)
                .filter(|budget| *budget > 0)
                .ok_or_else(|| invalid("thinking.budget_tokens", "expected a positive integer"))?;
            Ok(())
        }
        "adaptive" | "disabled" => Ok(()),
        other => Err(invalid(
            "thinking.type",
            &format!("unsupported thinking type `{other}` (enabled, adaptive, disabled)"),
        )),
    }
}

/// The role to stamp on a Responses `input` message item. Both endpoints accept
/// `user`, `assistant` and `developer`; codex rejects `system` ("System messages
/// are not allowed", verified live) and llmux applies the same folding on grok
/// as policy (no grok receipt either way). System-role messages never reach
/// here — they are folded into `instructions`.
///
/// The allowlist is closed: an unknown role used to be rewritten to `user`,
/// which silently changed WHO said something (a rewrite the client cannot see).
fn input_role(anthropic_role: &str) -> Option<&'static str> {
    match anthropic_role {
        "user" => Some("user"),
        "assistant" => Some("assistant"),
        "developer" => Some("developer"),
        _ => None,
    }
}

fn flush_text(parts: &mut Vec<Value>, text: &mut String, text_type: &str) {
    if !text.is_empty() {
        parts.push(json!({"type": text_type, "text": std::mem::take(text)}));
    }
}

fn flush_message(input: &mut Vec<Value>, parts: &mut Vec<Value>, role: &str) {
    if !parts.is_empty() {
        input.push(json!({
            "type": "message",
            "role": role,
            "content": std::mem::take(parts),
        }));
    }
}

/// A required non-empty string field of a content block.
fn nonempty_str<'a>(path: &str, block: &'a Value, field: &str) -> Result<&'a str, ProviderError> {
    block
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid(&format!("{path}.{field}"), "expected a non-empty string"))
}

fn convert_message(
    index: usize,
    message: &Value,
    count_tokens: bool,
    input: &mut Vec<Value>,
    folded_system: &mut Vec<String>,
    report: &mut CompatibilityReport,
) -> Result<(), ProviderError> {
    let path = format!("messages[{index}]");
    if !message.is_object() {
        return Err(invalid(&path, "expected an object"));
    }
    let role = match message.get("role") {
        None | Some(Value::Null) => "user",
        Some(Value::String(role)) => role.as_str(),
        Some(_) => return Err(invalid(&format!("{path}.role"), "expected a string")),
    };
    if role == "system" {
        let text = system_message_text(&path, message)?;
        if !text.is_empty() {
            folded_system.push(text);
        }
        return Ok(());
    }

    let out_role = input_role(role).ok_or_else(|| {
        invalid(
            &format!("{path}.role"),
            &format!("unsupported role `{role}` (user, assistant, developer, system)"),
        )
    })?;
    let text_type = if out_role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    let mut parts: Vec<Value> = Vec::new();
    let mut text = String::new();

    match message.get("content") {
        None | Some(Value::Null) => {}
        Some(Value::String(content)) => text.push_str(content),
        Some(Value::Array(blocks)) => {
            for (block_index, block) in blocks.iter().enumerate() {
                let bpath = format!("{path}.content[{block_index}]");
                let block_type = block.get("type").and_then(Value::as_str).ok_or_else(|| {
                    invalid(&bpath, "expected a content block with a string `type`")
                })?;
                match block_type {
                    "text" => {
                        let value = block.get("text").and_then(Value::as_str).ok_or_else(|| {
                            invalid(&format!("{bpath}.text"), "expected a string")
                        })?;
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(value);
                    }
                    "image" => {
                        let url = image_data_url(&bpath, block, role, count_tokens)?;
                        flush_text(&mut parts, &mut text, text_type);
                        parts.push(json!({"type": "input_image", "image_url": url}));
                    }
                    "tool_use" => {
                        let call_id = nonempty_str(&bpath, block, "id")?;
                        let name = nonempty_str(&bpath, block, "name")?;
                        // `input` is the tool's OWN arbitrary JSON — it is
                        // serialized verbatim and never walked as content.
                        // A MISSING `input` keeps the previous translator's
                        // `{}` (a no-arg call); an explicit `null` or a
                        // non-object is malformed and refused, because the old
                        // path turned it into the literal `"null"` arguments
                        // string the model then had to parse.
                        let arguments = match block.get("input") {
                            None => "{}".to_string(),
                            Some(value) if value.is_object() => value.to_string(),
                            Some(_) => {
                                return Err(invalid(
                                    &format!("{bpath}.input"),
                                    "expected a JSON object",
                                ))
                            }
                        };
                        flush_text(&mut parts, &mut text, text_type);
                        flush_message(input, &mut parts, out_role);
                        input.push(json!({
                            "type": "function_call",
                            "call_id": call_id,
                            "name": name,
                            "arguments": arguments,
                        }));
                    }
                    "tool_result" => {
                        let call_id = nonempty_str(&bpath, block, "tool_use_id")?;
                        let output = tool_result_output(&bpath, block, role, count_tokens)?;
                        flush_text(&mut parts, &mut text, text_type);
                        flush_message(input, &mut parts, out_role);
                        input.push(json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": output,
                        }));
                    }
                    // Prior-turn reasoning: an Anthropic `signature` is not
                    // either backend's reasoning ciphertext, and llmux
                    // implements NO replay/provenance bridge in v1 (neither
                    // backend's own `encrypted_content` is stored or replayed
                    // here). Passing a foreign signature through would assert a
                    // provenance llmux cannot back, so the block is omitted and
                    // reported; the rest of the transcript keeps its order.
                    "thinking" | "redacted_thinking" => {
                        if role != "assistant" {
                            return Err(invalid(
                                &bpath,
                                &format!("`{block_type}` is only valid on an `assistant` message"),
                            ));
                        }
                        report.omit(if block_type == "thinking" {
                            "thinking"
                        } else {
                            "redacted_thinking"
                        });
                    }
                    other => {
                        return Err(invalid(
                            &format!("{bpath}.type"),
                            &format!("unsupported content block type `{other}`"),
                        ))
                    }
                }
            }
        }
        Some(_) => {
            return Err(invalid(
                &format!("{path}.content"),
                "expected a string or an array of content blocks",
            ))
        }
    }
    flush_text(&mut parts, &mut text, text_type);
    flush_message(input, &mut parts, out_role);
    Ok(())
}

/// Text of a `role: "system"` message, for folding into `instructions`. Only
/// text blocks are foldable: an image (or anything else) in an operator message
/// has no instructions representation, so it is an error rather than a
/// disappearance.
fn system_message_text(path: &str, message: &Value) -> Result<String, ProviderError> {
    match message.get("content") {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Array(blocks)) => {
            let mut parts = Vec::new();
            for (index, block) in blocks.iter().enumerate() {
                let bpath = format!("{path}.content[{index}]");
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let text = block.get("text").and_then(Value::as_str).ok_or_else(|| {
                            invalid(&format!("{bpath}.text"), "expected a string")
                        })?;
                        parts.push(text.to_string());
                    }
                    Some(other) => {
                        return Err(invalid(
                            &format!("{bpath}.type"),
                            &format!("a system message carries text only (got `{other}`)"),
                        ))
                    }
                    None => {
                        return Err(invalid(
                            &bpath,
                            "expected a content block with a string `type`",
                        ))
                    }
                }
            }
            Ok(parts.join("\n"))
        }
        Some(_) => Err(invalid(
            &format!("{path}.content"),
            "expected a string or an array of text blocks",
        )),
    }
}

/// Compose `instructions` from the top-level `system` field plus any
/// system-role messages folded out of `messages[]`, in that order.
fn build_instructions(
    system: Option<&Value>,
    folded_system: &[String],
) -> Result<String, ProviderError> {
    let mut parts: Vec<String> = Vec::new();
    match system {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => {
            if !text.is_empty() {
                parts.push(text.clone());
            }
        }
        Some(Value::Array(blocks)) => {
            for (index, block) in blocks.iter().enumerate() {
                let bpath = format!("system[{index}]");
                match block.get("type").and_then(Value::as_str) {
                    // Absent `type` is tolerated for the historical
                    // `{"text": …}` shape; a NAMED non-text type is not.
                    Some("text") | None => {
                        let text = block.get("text").and_then(Value::as_str).ok_or_else(|| {
                            invalid(&format!("{bpath}.text"), "expected a string")
                        })?;
                        if !text.is_empty() {
                            parts.push(text.to_string());
                        }
                    }
                    Some(other) => {
                        return Err(invalid(
                            &format!("{bpath}.type"),
                            &format!("`system` carries text only (got `{other}`)"),
                        ))
                    }
                }
            }
        }
        Some(_) => {
            return Err(invalid(
                "system",
                "expected a string or an array of text blocks",
            ))
        }
    }
    parts.extend(folded_system.iter().filter(|s| !s.is_empty()).cloned());
    Ok(parts.join("\n"))
}

/// Anthropic `image` block → the `input_image.image_url` data URL both
/// endpoints accept (live receipt: base64 PNG answered on codex AND grok).
fn image_data_url(
    path: &str,
    block: &Value,
    role: &str,
    count_tokens: bool,
) -> Result<String, ProviderError> {
    if count_tokens {
        return Err(invalid(
            path,
            "image input has no reliable token estimate; count_tokens does not support it",
        ));
    }
    if role != "user" {
        return Err(invalid(
            path,
            &format!("image blocks are only supported on `user` messages (got role `{role}`)"),
        ));
    }
    let source = block
        .get("source")
        .filter(|source| source.is_object())
        .ok_or_else(|| invalid(&format!("{path}.source"), "expected an object"))?;
    let source_type = source
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("{path}.source.type"), "expected a string"))?;
    match source_type {
        "base64" => {
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    invalid(&format!("{path}.source.media_type"), "expected a string")
                })?;
            if !SUPPORTED_IMAGE_MEDIA_TYPES.contains(&media_type) {
                return Err(invalid(
                    &format!("{path}.source.media_type"),
                    &format!("unsupported image media type `{media_type}` (png and jpeg only)"),
                ));
            }
            let data = nonempty_str(&format!("{path}.source"), source, "data")?;
            // Reject an over-cap payload from its ENCODED length first, so a
            // huge blob is never decoded into memory to be refused.
            let decoded_len = data.len() / 4 * 3;
            if decoded_len > MAX_IMAGE_BYTES {
                return Err(oversized(path));
            }
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| {
                    // The payload itself never enters the message.
                    invalid(&format!("{path}.source.data"), "not valid base64")
                })?;
            if decoded.len() > MAX_IMAGE_BYTES {
                return Err(oversized(path));
            }
            Ok(format!("data:{media_type};base64,{data}"))
        }
        // Fetching the URL would make llmux an SSRF proxy for the client, and
        // the reference codex client refuses remote image URLs outright.
        "url" => Err(invalid(
            &format!("{path}.source.type"),
            "remote image URLs are not supported; send the image as base64",
        )),
        other => Err(invalid(
            &format!("{path}.source.type"),
            &format!("unsupported image source type `{other}`"),
        )),
    }
}

fn oversized(path: &str) -> ProviderError {
    invalid(
        path,
        &format!(
            "decoded image exceeds the {} MiB limit",
            MAX_IMAGE_BYTES / (1024 * 1024)
        ),
    )
}

/// `tool_result` → the `function_call_output.output` value: a plain string when
/// the result is text (the historical shape), or the content-item array both
/// endpoints accept when it carries images (live receipt: nested
/// `input_image` answered on codex AND grok).
fn tool_result_output(
    path: &str,
    block: &Value,
    role: &str,
    count_tokens: bool,
) -> Result<Value, ProviderError> {
    let is_error = match block.get("is_error") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(invalid(&format!("{path}.is_error"), "expected a boolean")),
    };

    let mut texts: Vec<String> = Vec::new();
    let mut parts: Vec<Value> = Vec::new();
    let mut has_image = false;
    let push_text = |text: String, parts: &mut Vec<Value>, texts: &mut Vec<String>| {
        texts.push(text.clone());
        parts.push(json!({"type": "input_text", "text": text}));
    };

    match block.get("content") {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => push_text(text.clone(), &mut parts, &mut texts),
        Some(Value::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                let ipath = format!("{path}.content[{index}]");
                let item_type = item.get("type").and_then(Value::as_str).ok_or_else(|| {
                    invalid(&ipath, "expected a content block with a string `type`")
                })?;
                match item_type {
                    "text" => {
                        let text = item.get("text").and_then(Value::as_str).ok_or_else(|| {
                            invalid(&format!("{ipath}.text"), "expected a string")
                        })?;
                        push_text(text.to_string(), &mut parts, &mut texts);
                    }
                    "image" => {
                        let url = image_data_url(&ipath, item, role, count_tokens)?;
                        has_image = true;
                        parts.push(json!({"type": "input_image", "image_url": url}));
                    }
                    other => {
                        return Err(invalid(
                            &format!("{ipath}.type"),
                            &format!("unsupported tool_result content type `{other}`"),
                        ))
                    }
                }
            }
        }
        Some(_) => {
            return Err(invalid(
                &format!("{path}.content"),
                "expected a string or an array of content blocks",
            ))
        }
    }

    if has_image {
        if is_error {
            parts.insert(
                0,
                json!({"type": "input_text", "text": TOOL_RESULT_ERROR_MARKER}),
            );
        }
        return Ok(Value::Array(parts));
    }
    let text = texts.join("\n");
    Ok(Value::String(if is_error {
        format!("{TOOL_RESULT_ERROR_MARKER}\n{text}")
    } else {
        text
    }))
}

/// Anthropic `tools[]` → Responses function tools. A nameless entry (a
/// server-side tool type llmux cannot execute or forward) is an error: dropping
/// it would leave the model announcing a capability that silently does not
/// exist.
fn convert_tools(tools: &Value) -> Result<Vec<Value>, ProviderError> {
    let tools = tools
        .as_array()
        .ok_or_else(|| invalid("tools", "expected an array"))?;
    let mut functions = Vec::with_capacity(tools.len());
    for (index, tool) in tools.iter().enumerate() {
        let path = format!("tools[{index}]");
        if !tool.is_object() {
            return Err(invalid(&path, "expected an object"));
        }
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                invalid(
                    &format!("{path}.name"),
                    "expected a non-empty string (server-side tools cannot be forwarded)",
                )
            })?;
        let mut function = Map::new();
        function.insert("type".into(), json!("function"));
        function.insert("name".into(), json!(name));
        match tool.get("description") {
            None | Some(Value::Null) => {}
            Some(Value::String(description)) => {
                function.insert("description".into(), json!(description));
            }
            Some(_) => return Err(invalid(&format!("{path}.description"), "expected a string")),
        }
        let parameters = match tool.get("input_schema") {
            None | Some(Value::Null) => json!({"type": "object", "properties": {}}),
            Some(schema) if schema.is_object() => schema.clone(),
            Some(_) => {
                return Err(invalid(
                    &format!("{path}.input_schema"),
                    "expected a JSON Schema object",
                ))
            }
        };
        function.insert("parameters".into(), parameters);
        function.insert("strict".into(), json!(false));
        functions.push(Value::Object(function));
    }
    Ok(functions)
}

/// Anthropic `tool_choice` → `(tool_choice, parallel_tool_calls)`.
/// `auto`→`"auto"`, `any`→`"required"`, `none`→`"none"`,
/// `tool{name}`→ the flat `{"type":"function","name":…}` selector (live receipt:
/// answered with a call to exactly that function on codex AND grok).
fn convert_tool_choice(
    choice: &Value,
    tool_names: &[&str],
) -> Result<(Option<Value>, Option<bool>), ProviderError> {
    if !choice.is_object() {
        return Err(invalid(
            "tool_choice",
            "expected an object such as {\"type\": \"auto\"}",
        ));
    }
    let parallel = match choice.get("disable_parallel_tool_use") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(disabled)) => Some(!disabled),
        Some(_) => {
            return Err(invalid(
                "tool_choice.disable_parallel_tool_use",
                "expected a boolean",
            ))
        }
    };
    let choice_type = choice
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("tool_choice.type", "expected a string"))?;
    let requires_tools = |kind: &str| {
        if tool_names.is_empty() {
            Err(invalid(
                "tool_choice",
                &format!("`{kind}` requires a non-empty `tools` array"),
            ))
        } else {
            Ok(())
        }
    };
    let translated = match choice_type {
        "auto" => json!("auto"),
        "any" => {
            requires_tools("any")?;
            json!("required")
        }
        "none" => json!("none"),
        "tool" => {
            requires_tools("tool")?;
            let name = choice
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| invalid("tool_choice.name", "expected a non-empty string"))?;
            if !tool_names.contains(&name) {
                return Err(invalid(
                    "tool_choice.name",
                    &format!("`{name}` is not among the request's tools"),
                ));
            }
            json!({"type": "function", "name": name})
        }
        other => {
            return Err(invalid(
                "tool_choice.type",
                &format!("unsupported tool_choice type `{other}`"),
            ))
        }
    };
    Ok((Some(translated), parallel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ResponsesFlavor::{Codex, Grok};

    /// A 1x1-ish base64 payload. The converter validates encoding + media type
    /// + size, never the pixels, so any valid base64 stands in for an image.
    const PNG_B64: &str = "aGVsbG8sIHBuZw==";

    fn plan(session: &str) -> RequestPlan<'_> {
        RequestPlan {
            upstream_model: "test-model",
            effort: None,
            priority_tier: false,
            include_encrypted_reasoning: false,
            session_id: session,
        }
    }

    fn build(body: &Value, flavor: ResponsesFlavor) -> Value {
        build_responses_body(body, &plan("sess"), flavor)
            .expect("build")
            .0
    }

    /// The `InvalidRequest` message for a body that must be refused.
    fn reject(body: &Value, flavor: ResponsesFlavor) -> String {
        match build_responses_body(body, &plan("sess"), flavor) {
            Err(ProviderError::InvalidRequest(message)) => message,
            Err(other) => panic!("expected InvalidRequest, got {other:?}"),
            Ok((upstream, _)) => panic!("expected InvalidRequest, got body {upstream}"),
        }
    }

    fn image_block(media_type: &str, data: &str) -> Value {
        json!({"type": "image", "source": {
            "type": "base64", "media_type": media_type, "data": data}})
    }

    fn report(body: &Value, flavor: ResponsesFlavor) -> CompatibilityReport {
        validate_request(body, flavor, false).expect("validate")
    }

    // ---- R1: images ----

    #[test]
    fn user_images_become_data_url_parts_in_transcript_order() {
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "text", "text": "before"},
            image_block("image/png", PNG_B64),
            {"type": "text", "text": "after"},
        ]}]});
        for flavor in [Codex, Grok] {
            let upstream = build(&body, flavor);
            let content = &upstream["input"][0]["content"];
            assert_eq!(
                upstream["input"].as_array().map(Vec::len),
                Some(1),
                "one message item carries the interleaved parts"
            );
            assert_eq!(content[0], json!({"type": "input_text", "text": "before"}));
            assert_eq!(
                content[1],
                json!({"type": "input_image",
                       "image_url": format!("data:image/png;base64,{PNG_B64}")}),
                "flat data-URL image_url, never the Chat-Completions nesting"
            );
            assert_eq!(
                content[2],
                json!({"type": "input_text", "text": "after"}),
                "text after the image keeps its position ({flavor:?})"
            );
        }
    }

    #[test]
    fn tool_result_images_become_a_nested_output_array() {
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "call_1", "content": [
                {"type": "text", "text": "screenshot:"},
                image_block("image/jpeg", PNG_B64),
            ]},
        ]}]});
        let upstream = build(&body, Codex);
        let item = &upstream["input"][0];
        assert_eq!(item["type"], "function_call_output");
        assert_eq!(item["call_id"], "call_1");
        assert_eq!(
            item["output"],
            json!([
                {"type": "input_text", "text": "screenshot:"},
                {"type": "input_image", "image_url": format!("data:image/jpeg;base64,{PNG_B64}")},
            ]),
            "the image inside a tool result survives instead of being flattened away"
        );
    }

    #[test]
    fn text_only_tool_result_keeps_the_plain_string_output() {
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "call_1",
             "content": [{"type": "text", "text": "22C"}]},
        ]}]});
        assert_eq!(build(&body, Codex)["input"][0]["output"], "22C");
    }

    #[test]
    fn remote_url_images_are_refused_rather_than_fetched() {
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "image", "source": {"type": "url", "url": "https://x.example/a.png"}},
        ]}]});
        let message = reject(&body, Codex);
        assert!(
            message.starts_with("messages[0].content[0].source.type:")
                && message.contains("base64"),
            "the error names the path and the remedy: {message:?}"
        );
    }

    #[test]
    fn images_outside_user_messages_are_refused() {
        let body = json!({"messages": [{"role": "assistant", "content": [
            image_block("image/png", PNG_B64),
        ]}]});
        assert!(reject(&body, Codex).contains("`user`"));
    }

    #[test]
    fn malformed_or_unsupported_images_are_refused_not_dropped() {
        let cases = [
            (image_block("image/gif", PNG_B64), "media_type"),
            (image_block("image/png", "not base64!!"), "data"),
            (json!({"type": "image"}), "source"),
        ];
        for (block, expected) in cases {
            let body = json!({"messages": [{"role": "user", "content": [block.clone()]}]});
            let message = reject(&body, Codex);
            assert!(
                message.contains(expected),
                "{block} should be refused on {expected}: {message:?}"
            );
        }
    }

    #[test]
    fn oversized_images_are_refused_by_decoded_size() {
        // 4 base64 chars = 3 bytes; one char past the cap is enough.
        let data = "A".repeat((MAX_IMAGE_BYTES / 3 + 1) * 4);
        let body = json!({"messages": [{"role": "user", "content": [
            image_block("image/png", &data),
        ]}]});
        let message = reject(&body, Codex);
        assert!(message.contains("20 MiB"), "{message:?}");
        assert!(
            !message.contains("AAAA"),
            "the payload must never appear in the error: {}",
            &message[..message.len().min(120)]
        );
    }

    #[test]
    fn unknown_content_blocks_are_refused_not_dropped() {
        for block_type in ["document", "audio", "video", "server_tool_use"] {
            let body = json!({"messages": [{"role": "user", "content": [
                {"type": block_type, "source": {"type": "base64", "data": "x"}},
            ]}]});
            let message = reject(&body, Codex);
            assert!(
                message.contains(block_type) && message.contains("messages[0].content[0].type"),
                "{block_type} must be a typed error: {message:?}"
            );
        }
    }

    #[test]
    fn tool_use_input_is_serialized_verbatim_and_never_walked_as_content() {
        // A tool's own arguments may legitimately contain keys that LOOK like
        // Anthropic content blocks; they are payload, not structure.
        let body = json!({"messages": [{"role": "assistant", "content": [
            {"type": "tool_use", "id": "call_1", "name": "render",
             "input": {"type": "image", "source": {"type": "url"}, "blocks": [{"type": "document"}]}},
        ]}]});
        let upstream = build(&body, Codex);
        let arguments: Value = serde_json::from_str(
            upstream["input"][0]["arguments"]
                .as_str()
                .expect("arguments"),
        )
        .expect("arguments json");
        assert_eq!(arguments["source"]["type"], "url");
        assert_eq!(arguments["blocks"][0]["type"], "document");
    }

    #[test]
    fn tool_blocks_require_their_identifiers() {
        let missing_id = json!({"messages": [{"role": "assistant", "content": [
            {"type": "tool_use", "name": "x", "input": {}},
        ]}]});
        assert!(reject(&missing_id, Codex).contains("content[0].id"));
        let missing_call = json!({"messages": [{"role": "user", "content": [
            {"type": "tool_result", "content": "42"},
        ]}]});
        assert!(reject(&missing_call, Codex).contains("tool_use_id"));
    }

    #[test]
    fn malformed_tool_use_input_is_refused_while_an_absent_one_stays_no_arg() {
        for bad in [json!(null), json!("{}"), json!([1, 2])] {
            let body = json!({"messages": [{"role": "assistant", "content": [
                {"type": "tool_use", "id": "c1", "name": "now", "input": bad},
            ]}]});
            assert!(
                reject(&body, Codex).starts_with("messages[0].content[0].input:"),
                "{bad} must be refused, never stringified into the arguments"
            );
        }
        // Backwards compatible: an ABSENT `input` is a genuine no-arg call.
        let absent = json!({"messages": [{"role": "assistant", "content": [
            {"type": "tool_use", "id": "c1", "name": "now"},
        ]}]});
        assert_eq!(build(&absent, Codex)["input"][0]["arguments"], "{}");
    }

    #[test]
    fn unknown_roles_are_refused_instead_of_being_rewritten_to_user() {
        let body = json!({"messages": [{"role": "tool", "content": "42"}]});
        let message = reject(&body, Codex);
        assert!(
            message.starts_with("messages[0].role:") && message.contains("tool"),
            "a rewritten speaker is a silent semantic change: {message:?}"
        );
        // The four roles llmux does carry still work.
        for role in ["user", "assistant", "developer"] {
            let body = json!({"messages": [{"role": role, "content": "hi"}]});
            assert_eq!(build(&body, Codex)["input"][0]["role"], role);
        }
        let system = json!({"messages": [{"role": "system", "content": "note"}]});
        let upstream = build(&system, Codex);
        assert_eq!(upstream["instructions"], "note");
        assert!(upstream["input"].as_array().expect("input").is_empty());
    }

    #[test]
    fn failed_tool_results_never_read_as_success() {
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "call_1", "is_error": true,
             "content": "ENOENT: /etc/nope"},
        ]}]});
        let output = build(&body, Codex)["input"][0]["output"]
            .as_str()
            .expect("string output")
            .to_string();
        assert!(
            output.starts_with(TOOL_RESULT_ERROR_MARKER),
            "the failure is explicit in the text: {output:?}"
        );
        assert!(output.contains("ENOENT: /etc/nope"), "{output:?}");
    }

    // ---- R2: tool_choice ----

    fn tool_body(choice: Value) -> Value {
        json!({
            "messages": [{"role": "user", "content": "go"}],
            "tools": [{"name": "report_color", "input_schema": {"type": "object"}},
                      {"name": "other", "input_schema": {"type": "object"}}],
            "tool_choice": choice,
        })
    }

    #[test]
    fn tool_choice_modes_map_to_the_responses_vocabulary() {
        for (anthropic, expected) in [
            (json!({"type": "auto"}), json!("auto")),
            (json!({"type": "any"}), json!("required")),
            (json!({"type": "none"}), json!("none")),
            (
                json!({"type": "tool", "name": "report_color"}),
                json!({"type": "function", "name": "report_color"}),
            ),
        ] {
            for flavor in [Codex, Grok] {
                let upstream = build(&tool_body(anthropic.clone()), flavor);
                assert_eq!(
                    upstream["tool_choice"], expected,
                    "{anthropic} under {flavor:?}"
                );
            }
        }
    }

    #[test]
    fn tool_choice_is_validated_instead_of_falling_back_to_auto() {
        let cases = [
            (tool_body(json!({"type": "tool", "name": "ghost"})), "ghost"),
            (tool_body(json!({"type": "mystery"})), "mystery"),
            (tool_body(json!("auto")), "tool_choice"),
            (
                json!({"messages": [{"role": "user", "content": "go"}],
                       "tool_choice": {"type": "any"}}),
                "requires a non-empty `tools`",
            ),
        ];
        for (body, expected) in cases {
            let message = reject(&body, Codex);
            assert!(message.contains(expected), "{expected} — got {message:?}");
        }
    }

    #[test]
    fn disable_parallel_tool_use_inverts_the_parallel_flag() {
        let on = build(&tool_body(json!({"type": "auto"})), Codex);
        assert_eq!(on["parallel_tool_calls"], true, "default stays parallel");
        let off = build(
            &tool_body(json!({"type": "auto", "disable_parallel_tool_use": true})),
            Codex,
        );
        assert_eq!(off["parallel_tool_calls"], false);
    }

    #[test]
    fn tool_less_requests_omit_the_whole_tool_trio() {
        // xAI rejects a `tool_choice` with no tools, and `auto`/`none` are
        // semantically vacuous there — so the trio goes together on BOTH
        // flavors rather than shipping an unmeasured `tools: []`.
        let body = json!({"messages": [{"role": "user", "content": "hi"}],
                          "tool_choice": {"type": "auto"}});
        for flavor in [Codex, Grok] {
            let upstream = build(&body, flavor);
            for field in ["tools", "tool_choice", "parallel_tool_calls"] {
                assert!(
                    upstream.get(field).is_none(),
                    "{field} must be absent under {flavor:?}: {upstream}"
                );
            }
        }
    }

    #[test]
    fn nameless_tools_are_refused_not_dropped() {
        let body = json!({"messages": [{"role": "user", "content": "hi"}],
                          "tools": [{"type": "web_search_20250305"}]});
        assert!(reject(&body, Codex).contains("tools[0].name"));
    }

    // ---- R3: max_tokens ----

    #[test]
    fn codex_omits_max_tokens_and_reports_the_omission() {
        let body = json!({"max_tokens": 1024, "messages": [{"role": "user", "content": "hi"}]});
        let upstream = build(&body, Codex);
        assert!(
            upstream.get("max_output_tokens").is_none(),
            "the codex backend 400s on max_output_tokens"
        );
        let report = report(&body, Codex);
        assert_eq!(report.omitted_fields, vec!["max_tokens"]);
        assert_eq!(report.warnings, vec!["max_tokens"]);
    }

    #[test]
    fn grok_forwards_max_tokens_with_a_semantics_warning() {
        let body = json!({"max_tokens": 1024, "messages": [{"role": "user", "content": "hi"}]});
        assert_eq!(build(&body, Grok)["max_output_tokens"], 1024);
        let report = report(&body, Grok);
        assert!(
            report.omitted_fields.is_empty(),
            "nothing is dropped: {report:?}"
        );
        assert_eq!(
            report.warnings,
            vec!["max_tokens_semantics"],
            "the cap is forwarded, its budget equivalence is unproven"
        );
    }

    #[test]
    fn explicit_null_max_tokens_is_refused_on_both_flavors() {
        // `"max_tokens": null` is an explicit limit that is not a positive
        // integer, so it takes the same path as `0` or `"16"`. Treating it as
        // "absent" would silently omit a field the client DID send — the exact
        // class of loss this module exists to stop.
        let body = json!({"max_tokens": null, "messages": [{"role": "user", "content": "hi"}]});
        for flavor in [Codex, Grok] {
            assert!(
                reject(&body, flavor).starts_with("max_tokens:"),
                "explicit null must be refused under {flavor:?}"
            );
            assert!(
                validate_request(&body, flavor, true).is_err(),
                "and on the count path too ({flavor:?})"
            );
        }
        // An ABSENT `max_tokens` stays legal: that is the no-cap body the
        // codex idle probe and `count_tokens` send.
        let absent = json!({"messages": [{"role": "user", "content": "hi"}]});
        for flavor in [Codex, Grok] {
            assert!(build(&absent, flavor).get("max_output_tokens").is_none());
            assert!(validate_request(&absent, flavor, true).is_ok());
        }
    }

    #[test]
    fn invalid_max_tokens_is_refused_rather_than_clamped() {
        for value in [json!(0), json!(-1), json!("16"), json!(1.5)] {
            let body = json!({"max_tokens": value,
                              "messages": [{"role": "user", "content": "hi"}]});
            assert!(
                reject(&body, Grok).starts_with("max_tokens:"),
                "{value} must be refused"
            );
        }
    }

    // ---- R4: prior thinking ----

    #[test]
    fn assistant_thinking_is_omitted_with_the_rest_of_the_turn_intact() {
        let body = json!({"messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hmm", "signature": "sig"},
                {"type": "text", "text": "a cat"},
                {"type": "redacted_thinking", "data": "opaque"},
            ]},
        ]});
        let upstream = build(&body, Codex);
        let input = upstream["input"].as_array().expect("input");
        assert_eq!(input.len(), 2, "user turn + assistant text: {upstream}");
        assert_eq!(input[1]["content"][0]["text"], "a cat");
        assert_eq!(
            input[1]["content"][0]["type"], "output_text",
            "assistant text keeps the output_text part type"
        );
        let report = report(&body, Codex);
        assert_eq!(
            report.omitted_fields,
            vec!["redacted_thinking", "thinking"],
            "both kinds are named, sorted and deduped"
        );
    }

    #[test]
    fn thinking_outside_an_assistant_message_is_refused() {
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "thinking", "thinking": "not mine"},
        ]}]});
        assert!(reject(&body, Codex).contains("assistant"));
    }

    // ---- R7: generation controls ----

    #[test]
    fn sampling_controls_are_refused_instead_of_quietly_ignored() {
        // Neither subscription endpoint's acceptance of these is verified, and
        // the previous translator read none of them: the request simply ran at
        // the backend default while the client believed it had set one.
        for (field, value) in [
            ("temperature", json!(0.2)),
            ("top_p", json!(0.9)),
            ("top_k", json!(40)),
        ] {
            let mut body = json!({"messages": [{"role": "user", "content": "hi"}]});
            body[field] = value;
            for flavor in [Codex, Grok] {
                let message = reject(&body, flavor);
                assert!(
                    message.starts_with(&format!("{field}:")),
                    "{field} under {flavor:?}: {message:?}"
                );
            }
            // An explicit `null` sets nothing, so it is not a refused control.
            let mut nulled = json!({"messages": [{"role": "user", "content": "hi"}]});
            nulled[field] = json!(null);
            assert!(build(&nulled, Codex).get(field).is_none());
        }
    }

    #[test]
    fn stop_sequences_are_refused_unless_vacuous() {
        let with = json!({"stop_sequences": ["\n\nHuman:"],
                          "messages": [{"role": "user", "content": "hi"}]});
        assert!(reject(&with, Codex).starts_with("stop_sequences:"));
        let malformed = json!({"stop_sequences": "\n\nHuman:",
                               "messages": [{"role": "user", "content": "hi"}]});
        assert!(reject(&malformed, Grok).starts_with("stop_sequences:"));
        // An EMPTY array asks for nothing — no reason to fail the request.
        let empty = json!({"stop_sequences": [],
                           "messages": [{"role": "user", "content": "hi"}]});
        assert!(report(&empty, Codex).is_empty());
        assert!(build(&empty, Codex).get("stop_sequences").is_none());
    }

    #[test]
    fn thinking_config_is_validated_and_reported_never_enforced() {
        for thinking in [
            json!({"type": "enabled", "budget_tokens": 16000}),
            json!({"type": "adaptive"}),
            json!({"type": "disabled"}),
        ] {
            let body = json!({"thinking": thinking,
                              "messages": [{"role": "user", "content": "hi"}]});
            for flavor in [Codex, Grok] {
                let upstream = build(&body, flavor);
                assert!(
                    upstream.get("thinking").is_none() && upstream.get("reasoning").is_none(),
                    "no fabricated budget/effort mapping: {upstream}"
                );
                assert_eq!(
                    report(&body, flavor).omitted_fields,
                    vec!["thinking_config"],
                    "{thinking} under {flavor:?} is a reported omission, not a silent one"
                );
                // Counting sends no generation control upstream, so it reports
                // no omission — but the shape is still checked.
                assert!(validate_request(&body, flavor, true)
                    .expect("count validates")
                    .is_empty());
            }
        }
    }

    #[test]
    fn malformed_thinking_config_is_refused() {
        for thinking in [
            json!({"type": "enabled"}),
            json!({"type": "enabled", "budget_tokens": 0}),
            json!({"type": "enabled", "budget_tokens": "16000"}),
            json!({"type": "sometimes"}),
            json!({}),
            json!("enabled"),
        ] {
            let body = json!({"thinking": thinking,
                              "messages": [{"role": "user", "content": "hi"}]});
            assert!(
                reject(&body, Codex).starts_with("thinking"),
                "{thinking} must be refused"
            );
            assert!(validate_request(&body, Codex, true).is_err(), "{thinking}");
        }
    }

    // ---- R5: the report itself ----

    #[test]
    fn warnings_are_the_superset_of_omissions_sorted_and_deduped() {
        let body = json!({
            "max_tokens": 64,
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "one"},
                    {"type": "text", "text": "a"},
                ]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "two"},
                    {"type": "text", "text": "b"},
                ]},
            ],
        });
        let codex = report(&body, Codex);
        assert_eq!(codex.omitted_fields, vec!["max_tokens", "thinking"]);
        assert_eq!(codex.warnings, vec!["max_tokens", "thinking"]);
        let grok = report(&body, Grok);
        assert_eq!(grok.omitted_fields, vec!["thinking"]);
        assert_eq!(grok.warnings, vec!["max_tokens_semantics", "thinking"]);
        assert!(!grok.is_empty());
        let clean = report(
            &json!({"messages": [{"role": "user", "content": "hi"}]}),
            Grok,
        );
        assert!(clean.is_empty(), "a plain text turn loses nothing");
    }

    // ---- R6: count_tokens mode ----

    #[test]
    fn count_mode_refuses_images_and_skips_inference_only_warnings() {
        let with_image = json!({"max_tokens": 32, "messages": [{"role": "user", "content": [
            image_block("image/png", PNG_B64),
        ]}]});
        let err = validate_request(&with_image, Codex, true).expect_err("count rejects images");
        let ProviderError::InvalidRequest(message) = err else {
            panic!("expected InvalidRequest");
        };
        assert!(message.contains("token estimate"), "{message:?}");

        let text_only = json!({"max_tokens": 32, "messages": [{"role": "user", "content": "hi"}]});
        for flavor in [Codex, Grok] {
            let report = validate_request(&text_only, flavor, true).expect("count validates");
            assert!(
                report.is_empty(),
                "counting sends no output budget, so no cap warning ({flavor:?}): {report:?}"
            );
        }
        assert!(
            validate_request(&json!({"model": "m"}), Codex, true).is_err(),
            "a body without messages cannot be counted"
        );
    }

    // ---- R7: preserved behavior ----

    #[test]
    fn text_and_tool_round_trip_matches_the_previous_translator() {
        let body = json!({
            "stream": true,
            "system": "be terse",
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "system", "content": "operator note"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "checking"},
                    {"type": "tool_use", "id": "call_1", "name": "get_weather",
                     "input": {"city": "Seoul"}},
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "22C"},
                ]},
            ],
            "tools": [{"name": "get_weather", "description": "Get weather",
                       "input_schema": {"type": "object", "properties": {}}}],
        });
        let (upstream, client_stream) =
            build_responses_body(&body, &plan("sess-1"), Codex).expect("build");
        assert!(client_stream);
        assert_eq!(upstream["instructions"], "be terse\noperator note");
        assert_eq!(upstream["store"], false);
        assert_eq!(upstream["stream"], true);
        assert_eq!(upstream["model"], "test-model");
        assert_eq!(upstream["prompt_cache_key"], "sess-1");
        let input = upstream["input"].as_array().expect("input");
        assert_eq!(input.len(), 4, "user text, assistant text, call, output");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[1]["content"][0]["text"], "checking");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[2]["name"], "get_weather");
        assert_eq!(input[2]["arguments"], "{\"city\":\"Seoul\"}");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["output"], "22C");
        assert!(
            !input.iter().any(|item| item["role"] == "system"),
            "system folds into instructions, never an input item"
        );
        let tools = upstream["tools"].as_array().expect("tools");
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["strict"], false);
        assert_eq!(tools[0]["parameters"]["type"], "object");
    }

    #[test]
    fn grok_omits_the_process_wide_prompt_cache_key() {
        let body = json!({"messages": [{"role": "user", "content": "hi"}]});
        assert!(
            build(&body, Grok).get("prompt_cache_key").is_none(),
            "the key's routing scope on cli-chat-proxy is undocumented"
        );
        assert_eq!(build(&body, Codex)["prompt_cache_key"], "sess");
    }

    #[test]
    fn structurally_broken_bodies_are_typed_errors() {
        let cases = [
            (json!({"model": "m"}), "messages"),
            (json!({"messages": "hi"}), "messages"),
            (json!({"messages": [42]}), "messages[0]"),
            (
                json!({"messages": [{"role": "user", "content": 7}]}),
                "messages[0].content",
            ),
            (
                json!({"messages": [{"role": "user", "content": [{"text": "no type"}]}]}),
                "messages[0].content[0]",
            ),
            (
                json!({"messages": [{"role": "user", "content": "hi"}], "tools": "all"}),
                "tools",
            ),
            (
                json!({"messages": [{"role": "user", "content": "hi"}],
                       "system": [{"type": "image"}]}),
                "system[0].type",
            ),
        ];
        for (body, expected) in cases {
            let message = reject(&body, Codex);
            assert!(
                message.starts_with(expected),
                "{body} should fail on {expected}: {message:?}"
            );
        }
    }
}
