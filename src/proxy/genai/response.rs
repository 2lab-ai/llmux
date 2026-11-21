//! The genai return boundary: whatever the shared loop answered, back in the
//! public Gemini API shape.
//!
//! The loop answers a genai request with one of two SSE streams:
//!
//! - **Anthropic Messages SSE** (claude through the Agent SDK, and codex /
//!   grok / openrouter through their Messages arms) — converted here, event by
//!   event ([`FromMessages`]).
//! - **Native genai frames** (the gemini group) — the Cloud Code stream with
//!   its `{"response":…}` wrapper already removed by [`NativeStream`] in the
//!   relay, `thoughtSignature`s and all. Relayed as they are.
//!
//! A Messages event always carries a `type`; a genai frame never does, so
//! each event says which it is. Frames go out as `data: <json>\r\n\r\n` (the
//! public API's framing); `generateContent` folds them into one response
//! ([`Aggregate`]). Non-2xx answers become Google errors (`retry-after`
//! kept); a stream that fails mid-way ends in an `{"error":…}` frame.

use axum::{body::Body, response::Response};
use bytes::Bytes;
use http::{header, StatusCode};
use serde_json::{json, Value};
use tokio_stream::StreamExt;

use super::super::sse::{EventBuffer, SseTransform, StreamUsage};
use super::{error, error_object, Method};
use crate::proxy::ingress::Capture;

/// One genai SSE frame.
fn frame(value: &Value) -> Vec<u8> {
    format!("data: {value}\r\n\r\n").into_bytes()
}

/// The concatenated `data:` payload of one SSE event (`None` for comments).
fn event_data(event: &str) -> Option<String> {
    let lines: Vec<&str> = event
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The Google error object for a mid-stream failure of `kind` (the
/// Anthropic error type, when known).
fn stream_error(kind: Option<&str>, message: &str) -> Value {
    let status = match kind {
        Some("overloaded_error") => StatusCode::SERVICE_UNAVAILABLE,
        Some("rate_limit_error") => StatusCode::TOO_MANY_REQUESTS,
        Some("invalid_request_error") => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    json!({"error": error_object(status, message)})
}

/// One open Anthropic content block (text and thinking stream straight
/// through; a tool call is held until its block stops).
#[derive(Debug)]
enum Block {
    Streamed,
    ToolUse {
        id: String,
        name: String,
        args: String,
    },
}

/// Anthropic Messages SSE → genai frames. One per response.
#[derive(Debug)]
pub(crate) struct FromMessages {
    model: String,
    response_id: String,
    blocks: std::collections::HashMap<u64, Block>,
    input: u64,
    cache_read: u64,
    cache_creation: u64,
    output: u64,
    stop_reason: Option<String>,
    done: bool,
}

impl FromMessages {
    pub(crate) fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            response_id: format!("llmux-{}", ulid::Ulid::new().to_string().to_lowercase()),
            blocks: std::collections::HashMap::new(),
            input: 0,
            cache_read: 0,
            cache_creation: 0,
            output: 0,
            stop_reason: None,
            done: false,
        }
    }

    fn content(&self, part: Value) -> Value {
        json!({
            "candidates": [{"content": {"role": "model", "parts": [part]}, "index": 0}],
            "modelVersion": self.model,
            "responseId": self.response_id,
        })
    }

    fn usage(&self) -> Value {
        let prompt = self.input + self.cache_read + self.cache_creation;
        let mut usage = json!({
            "promptTokenCount": prompt,
            "candidatesTokenCount": self.output,
            "totalTokenCount": prompt + self.output,
        });
        if self.cache_read > 0 {
            usage["cachedContentTokenCount"] = json!(self.cache_read);
        }
        usage
    }

    fn fail(&mut self, out: &mut Vec<Value>, kind: Option<&str>, message: &str) {
        if !self.done {
            self.done = true;
            out.push(stream_error(kind, message));
        }
    }

    fn record_usage(&mut self, usage: &Value) {
        let count = |key: &str| usage.get(key).and_then(Value::as_u64);
        if let Some(input) = count("input_tokens") {
            self.input = input;
        }
        if let Some(read) = count("cache_read_input_tokens") {
            self.cache_read = read;
        }
        if let Some(created) = count("cache_creation_input_tokens") {
            self.cache_creation = created;
        }
        if let Some(output) = count("output_tokens") {
            self.output = output;
        }
    }

    /// One Messages event → zero or more genai frames.
    pub(crate) fn event(&mut self, v: &Value, out: &mut Vec<Value>) {
        if self.done {
            return;
        }
        let index = v["index"].as_u64().unwrap_or(0);
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                let message = &v["message"];
                if let Some(model) = message["model"].as_str().filter(|m| !m.is_empty()) {
                    self.model = model.to_string();
                }
                if let Some(id) = message["id"].as_str().filter(|id| !id.is_empty()) {
                    self.response_id = id.to_string();
                }
                self.record_usage(&message["usage"]);
            }
            "content_block_start" => {
                let block = &v["content_block"];
                let opened = match block["type"].as_str() {
                    Some("text") => {
                        if let Some(text) = block["text"].as_str().filter(|t| !t.is_empty()) {
                            out.push(self.content(json!({"text": text})));
                        }
                        Block::Streamed
                    }
                    Some("thinking") => {
                        if let Some(text) = block["thinking"].as_str().filter(|t| !t.is_empty()) {
                            out.push(self.content(json!({"text": text, "thought": true})));
                        }
                        Block::Streamed
                    }
                    Some("tool_use") => Block::ToolUse {
                        id: block["id"].as_str().unwrap_or("").to_string(),
                        name: block["name"].as_str().unwrap_or("").to_string(),
                        args: String::new(),
                    },
                    _ => return,
                };
                self.blocks.insert(index, opened);
            }
            "content_block_delta" => {
                // Text and thinking deltas stand on their own (a relay that
                // skipped the block start still delivers its text); tool
                // arguments need their open `tool_use` block.
                let delta = &v["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        if let Some(text) = delta["text"].as_str().filter(|t| !t.is_empty()) {
                            let frame = self.content(json!({"text": text}));
                            out.push(frame);
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta["thinking"].as_str().filter(|t| !t.is_empty()) {
                            let frame = self.content(json!({"text": text, "thought": true}));
                            out.push(frame);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(Block::ToolUse { args, .. }) = self.blocks.get_mut(&index) {
                            args.push_str(delta["partial_json"].as_str().unwrap_or(""));
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(Block::ToolUse { id, name, args }) = self.blocks.remove(&index) {
                    let args = if args.trim().is_empty() {
                        Ok(json!({}))
                    } else {
                        serde_json::from_str::<Value>(&args)
                    };
                    match args {
                        Ok(args) if args.is_object() => {
                            let mut call = json!({"name": name, "args": args});
                            if !id.is_empty() {
                                call["id"] = json!(id);
                            }
                            out.push(self.content(json!({"functionCall": call})));
                        }
                        _ => self.fail(
                            out,
                            None,
                            &format!("malformed arguments for tool call {name:?}"),
                        ),
                    }
                }
            }
            "message_delta" => {
                self.record_usage(&v["usage"]);
                if let Some(reason) = v["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(reason.to_string());
                }
            }
            "message_stop" => {
                let finish = match self.stop_reason.as_deref() {
                    Some("max_tokens" | "model_context_window_exceeded") => "MAX_TOKENS",
                    Some("refusal") => "SAFETY",
                    _ => "STOP",
                };
                let mut last = self.content(json!({"text": ""}));
                last["candidates"][0]["finishReason"] = json!(finish);
                last["usageMetadata"] = self.usage();
                out.push(last);
                self.done = true;
            }
            "error" => {
                let kind = v["error"]["type"].as_str().map(str::to_string);
                let message = v["error"]["message"]
                    .as_str()
                    .unwrap_or("upstream error")
                    .to_string();
                self.fail(out, kind.as_deref(), &message);
            }
            _ => {}
        }
    }
}

/// Whether a genai frame ends the response: a `finishReason`, a blocked
/// prompt, or an error.
fn is_terminal(frame: &Value) -> bool {
    frame.get("error").is_some()
        || frame
            .pointer("/candidates/0/finishReason")
            .and_then(Value::as_str)
            .is_some_and(|reason| !reason.is_empty())
        || frame.pointer("/promptFeedback/blockReason").is_some()
}

/// `generateContent`: every frame of a stream folded into one response.
#[derive(Debug, Default)]
struct Aggregate {
    parts: Vec<Value>,
    candidate: serde_json::Map<String, Value>,
    top: serde_json::Map<String, Value>,
    error: Option<Value>,
}

impl Aggregate {
    fn push(&mut self, frame: &Value) {
        if let Some(error) = frame.get("error") {
            self.error.get_or_insert_with(|| error.clone());
            return;
        }
        if let Some(object) = frame.as_object() {
            for (key, value) in object {
                if key != "candidates" {
                    self.top.insert(key.clone(), value.clone());
                }
            }
        }
        let Some(candidate) = frame.pointer("/candidates/0").and_then(Value::as_object) else {
            return;
        };
        for (key, value) in candidate {
            if key != "content" {
                self.candidate.insert(key.clone(), value.clone());
            }
        }
        for part in candidate
            .get("content")
            .and_then(|c| c["parts"].as_array())
            .into_iter()
            .flatten()
        {
            self.push_part(part);
        }
    }

    /// Adjacent text parts of the same kind merge (a signature rides on the
    /// merged part); every other part is kept as it came.
    fn push_part(&mut self, part: &Value) {
        if let (Some(text), Some(last)) = (part["text"].as_str(), self.parts.last_mut()) {
            if last["text"].is_string()
                && last["thought"] == part["thought"]
                && last.get("thoughtSignature").is_none()
            {
                let merged = format!("{}{text}", last["text"].as_str().unwrap_or(""));
                last["text"] = json!(merged);
                if let Some(signature) = part.get("thoughtSignature") {
                    last["thoughtSignature"] = signature.clone();
                }
                return;
            }
        }
        if part["text"] == "" && part.get("thoughtSignature").is_none() {
            return;
        }
        self.parts.push(part.clone());
    }

    /// The one response, or the error (status, Google error object).
    fn finish(self) -> Result<Value, (StatusCode, Value)> {
        if let Some(error) = self.error {
            let status = error["code"]
                .as_u64()
                .and_then(|code| u16::try_from(code).ok())
                .and_then(|code| StatusCode::from_u16(code).ok())
                .filter(|status| !status.is_success())
                .unwrap_or(StatusCode::BAD_GATEWAY);
            return Err((status, error));
        }
        let mut candidate = Value::Object(self.candidate);
        candidate["content"] = json!({"role": "model", "parts": self.parts});
        candidate["index"] = json!(0);
        let mut response = Value::Object(self.top);
        response["candidates"] = json!([candidate]);
        Ok(response)
    }
}

/// Convert the shared loop's response for a genai request of `method` back to
/// the Gemini API, capturing both sides at this boundary when `capture` is set.
pub(crate) async fn adapt(
    response: Response,
    model: String,
    method: Method,
    capture: Option<Capture>,
) -> Response {
    if !response.status().is_success() {
        return adapt_error(response, capture).await;
    }
    let mut headers = response.headers().clone();
    headers.remove(header::CONTENT_LENGTH);
    let streaming = method.streams();
    let content_type = if streaming {
        "text/event-stream"
    } else {
        "application/json"
    };
    headers.insert(
        header::CONTENT_TYPE,
        http::HeaderValue::from_static(content_type),
    );
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    let mut stream = response.into_body().into_data_stream();
    let capture_headers = headers.clone();
    let task = tokio::spawn(async move {
        let limit = capture.as_ref().map(|c| c.max_body).unwrap_or(0);
        let mut upstream_capture = super::super::sse::RawCapture::new(limit);
        let mut output_capture = super::super::sse::RawCapture::new(limit);
        let mut buffer = EventBuffer::default();
        let mut converter = FromMessages::new(&model);
        let mut aggregate = Aggregate::default();
        let mut terminal = false;
        loop {
            let next = tokio::select! { _ = tx.closed() => break, next = stream.next() => next };
            let Some(Ok(chunk)) = next else {
                break;
            };
            upstream_capture.push(&chunk);
            let mut frames = Vec::new();
            for event in buffer.push(&chunk) {
                let Some(data) = event_data(&event) else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
                    continue;
                };
                if value.get("type").is_some() {
                    converter.event(&value, &mut frames);
                } else if !terminal {
                    frames.push(value);
                }
                terminal |= frames.iter().any(is_terminal);
            }
            let mut out = Vec::new();
            for value in &frames {
                if streaming {
                    out.extend(frame(value));
                } else {
                    aggregate.push(value);
                }
            }
            if !out.is_empty() {
                output_capture.push(&out);
                if tx.send(Ok(Bytes::from(out))).await.is_err() {
                    break;
                }
            }
        }
        if !terminal {
            let failure = json!({"error": error_object(
                StatusCode::BAD_GATEWAY,
                "upstream stream ended before a terminal response",
            )});
            if streaming {
                let out = frame(&failure);
                output_capture.push(&out);
                let _ = tx.send(Ok(Bytes::from(out))).await;
            } else {
                aggregate.push(&failure);
            }
        }
        let mut status = StatusCode::OK;
        if !streaming {
            let body = match aggregate.finish() {
                Ok(response) => response,
                Err((failed, error)) => {
                    status = failed;
                    json!({"error": error})
                }
            };
            let data = body.to_string();
            output_capture.push(data.as_bytes());
            let _ = tx.send(Ok(Bytes::from(data))).await;
        }
        if let Some(capture) = capture {
            capture.finish(status, &upstream_capture, &output_capture, &capture_headers);
        }
        status
    });
    let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    if streaming {
        let mut out = Response::new(body);
        *out.headers_mut() = headers;
        return out;
    }
    let data = axum::body::to_bytes(body, 64 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let status = task.await.unwrap_or(StatusCode::BAD_GATEWAY);
    let mut out = Response::new(Body::from(data));
    *out.headers_mut() = headers;
    *out.status_mut() = status;
    out
}

/// A non-2xx answer as a Google error: the message the answer carried
/// (credentials masked), the same status, `retry-after` kept.
async fn adapt_error(response: Response, capture: Option<Capture>) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let detail = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| value["detail"].as_str())
        .or_else(|| value["message"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let text = String::from_utf8_lossy(&bytes);
            let text = text.trim();
            if text.is_empty() {
                format!("llmux upstream failure ({status})")
            } else {
                text.chars().take(2000).collect()
            }
        });
    let detail = super::super::logging::mask_credentials(&detail);
    let mut out = error(status, &detail);
    if let Some(retry) = headers.get(header::RETRY_AFTER) {
        out.headers_mut().insert(header::RETRY_AFTER, retry.clone());
    }
    if let Some(capture) = capture {
        let mut upstream = super::super::sse::RawCapture::new(capture.max_body);
        upstream.push(&bytes);
        let mut output = super::super::sse::RawCapture::new(capture.max_body);
        output.push(
            json!({"error": error_object(status, &detail)})
                .to_string()
                .as_bytes(),
        );
        capture.finish(status, &upstream, &output, out.headers());
    }
    out
}

/// The gemini group's own stream, relayed natively: Cloud Code
/// `streamGenerateContent` events (`{"response": GenerateContentResponse,
/// "traceId", …}`) in, public genai frames out — the `response` member
/// unwrapped, every part (thoughts, `thoughtSignature`s, function calls with
/// Google's ids) untouched. Usage is read the way the Messages converter
/// reads it (last `usageMetadata` wins; fresh input = prompt − cached).
/// A stream that ends without a `finishReason` (or a blocked prompt) ends in
/// an error frame.
#[derive(Debug, Default)]
pub(crate) struct NativeStream {
    usage: StreamUsage,
    raw_usage: Option<Value>,
    events: u64,
    terminal: bool,
    error: Option<String>,
}

impl NativeStream {
    pub(crate) fn error_message(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn raw_usage(&self) -> Option<&Value> {
        self.raw_usage.as_ref()
    }

    pub(crate) fn events_seen(&self) -> u64 {
        self.events
    }

    fn record_usage(&mut self, usage: &Value) {
        let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let prompt = count("promptTokenCount");
        let cached = count("cachedContentTokenCount").min(prompt);
        self.raw_usage = Some(usage.clone());
        self.usage = StreamUsage {
            input_tokens: prompt - cached,
            output_tokens: count("candidatesTokenCount")
                .saturating_add(count("thoughtsTokenCount")),
            cache_read_input_tokens: Some(cached),
            ..StreamUsage::default()
        };
    }

    fn fail(&mut self, message: &str, status: StatusCode) -> Vec<u8> {
        self.terminal = true;
        self.error = Some(message.to_string());
        frame(&json!({"error": error_object(status, message)}))
    }
}

impl SseTransform for NativeStream {
    fn on_event(&mut self, event: &str) -> Vec<u8> {
        if self.error.is_some() {
            return Vec::new();
        }
        let Some(data) = event_data(event) else {
            return Vec::new();
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            tracing::debug!(provider = "gemini", "unparseable upstream SSE data dropped");
            return Vec::new();
        };
        self.events += 1;
        if let Some(error) = value.get("error") {
            let message = error["message"]
                .as_str()
                .or_else(|| error.as_str())
                .unwrap_or("gemini upstream error")
                .to_string();
            let status = error["code"]
                .as_u64()
                .and_then(|code| u16::try_from(code).ok())
                .and_then(|code| StatusCode::from_u16(code).ok())
                .filter(|status| !status.is_success())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            return self.fail(&message, status);
        }
        let response = value.get("response").unwrap_or(&value);
        if let Some(usage) = response.get("usageMetadata") {
            self.record_usage(usage);
        }
        self.terminal |= is_terminal(response);
        frame(response)
    }

    fn on_end(&mut self) -> Vec<u8> {
        if self.terminal {
            return Vec::new();
        }
        let message = if self.events == 0 {
            "gemini upstream returned no SSE events"
        } else {
            "gemini upstream stream ended before a finishReason"
        };
        self.fail(message, StatusCode::BAD_GATEWAY)
    }

    fn usage(&self) -> StreamUsage {
        self.usage.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages_sse(events: &[Value]) -> Response {
        let body: String = events
            .iter()
            .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
            .collect();
        Response::new(Body::from(body))
    }

    async fn body_of(response: Response) -> String {
        String::from_utf8(
            axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    }

    fn frames(text: &str) -> Vec<Value> {
        assert!(text.ends_with("\r\n\r\n"), "{text:?}");
        text.split("\r\n\r\n")
            .filter(|f| !f.is_empty())
            .map(|f| serde_json::from_str(f.strip_prefix("data: ").unwrap()).unwrap())
            .collect()
    }

    fn turn(stop: &str) -> Vec<Value> {
        vec![
            json!({"type":"message_start","message":{"id":"msg_1","model":"claude-opus-5-5","usage":{"input_tokens":10,"cache_read_input_tokens":80,"cache_creation_input_tokens":10,"output_tokens":1}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"s"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hel"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"lo"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"run_command"}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"CommandLine\":"}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"ls\"}"}}),
            json!({"type":"content_block_stop","index":2}),
            json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":42}}),
            json!({"type":"message_stop"}),
        ]
    }

    #[tokio::test]
    async fn messages_stream_becomes_genai_frames() {
        let out = adapt(
            messages_sse(&turn("tool_use")),
            "m".into(),
            Method::StreamGenerateContent,
            None,
        )
        .await;
        assert_eq!(out.status(), StatusCode::OK);
        assert_eq!(out.headers()[header::CONTENT_TYPE], "text/event-stream");
        let frames = frames(&body_of(out).await);
        let part = |i: usize| frames[i]["candidates"][0]["content"]["parts"][0].clone();
        assert_eq!(part(0), json!({"text": "hmm", "thought": true}));
        assert_eq!(part(1), json!({"text": "Hel"}));
        assert_eq!(part(2), json!({"text": "lo"}));
        assert_eq!(
            part(3),
            json!({"functionCall": {"name": "run_command", "args": {"CommandLine": "ls"}, "id": "toolu_1"}})
        );
        assert_eq!(frames[0]["candidates"][0]["content"]["role"], "model");
        assert_eq!(frames[0]["modelVersion"], "claude-opus-5-5");
        assert_eq!(frames[0]["responseId"], "msg_1");
        let last = frames.last().unwrap();
        assert_eq!(last["candidates"][0]["finishReason"], "STOP");
        assert_eq!(
            last["usageMetadata"],
            json!({"promptTokenCount": 100, "cachedContentTokenCount": 80,
                   "candidatesTokenCount": 42, "totalTokenCount": 142})
        );
        assert_eq!(frames.len(), 5);
    }

    #[tokio::test]
    async fn stop_reasons_map_to_finish_reasons() {
        for (stop, finish) in [
            ("end_turn", "STOP"),
            ("tool_use", "STOP"),
            ("max_tokens", "MAX_TOKENS"),
            ("refusal", "SAFETY"),
        ] {
            let out = adapt(
                messages_sse(&turn(stop)),
                "m".into(),
                Method::StreamGenerateContent,
                None,
            )
            .await;
            let frames = frames(&body_of(out).await);
            assert_eq!(
                frames.last().unwrap()["candidates"][0]["finishReason"],
                finish,
                "{stop}"
            );
        }
    }

    #[tokio::test]
    async fn generate_content_aggregates_one_response() {
        let out = adapt(
            messages_sse(&turn("end_turn")),
            "m".into(),
            Method::GenerateContent,
            None,
        )
        .await;
        assert_eq!(out.status(), StatusCode::OK);
        assert_eq!(out.headers()[header::CONTENT_TYPE], "application/json");
        let body: Value = serde_json::from_str(&body_of(out).await).unwrap();
        assert_eq!(
            body["candidates"][0]["content"]["parts"],
            json!([
                {"text": "hmm", "thought": true},
                {"text": "Hello"},
                {"functionCall": {"name": "run_command", "args": {"CommandLine": "ls"}, "id": "toolu_1"}},
            ])
        );
        assert_eq!(body["candidates"][0]["finishReason"], "STOP");
        assert_eq!(body["usageMetadata"]["totalTokenCount"], 142);
        assert_eq!(body["modelVersion"], "claude-opus-5-5");
    }

    #[tokio::test]
    async fn failures_end_in_a_google_error() {
        // A mid-stream Anthropic error → an error frame, then nothing.
        let events = vec![
            json!({"type":"message_start","message":{"usage":{"input_tokens":1}}}),
            json!({"type":"error","error":{"type":"overloaded_error","message":"busy"}}),
        ];
        let out = adapt(
            messages_sse(&events),
            "m".into(),
            Method::StreamGenerateContent,
            None,
        )
        .await;
        assert_eq!(
            frames(&body_of(out).await),
            vec![json!({"error": {"code": 503, "message": "busy", "status": "UNAVAILABLE"}})]
        );
        // A stream that just stops: streamed → error frame; aggregated → 502.
        let events = vec![json!({"type":"message_start","message":{"usage":{"input_tokens":1}}})];
        let out = adapt(
            messages_sse(&events),
            "m".into(),
            Method::StreamGenerateContent,
            None,
        )
        .await;
        assert_eq!(
            frames(&body_of(out).await)[0]["error"]["status"],
            "UNAVAILABLE"
        );
        let out = adapt(
            messages_sse(&events),
            "m".into(),
            Method::GenerateContent,
            None,
        )
        .await;
        assert_eq!(out.status(), StatusCode::BAD_GATEWAY);
        // Malformed tool arguments never reach the client as a call.
        let events = vec![
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"f"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_stop"}),
        ];
        let out = adapt(
            messages_sse(&events),
            "m".into(),
            Method::GenerateContent,
            None,
        )
        .await;
        assert_eq!(out.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body: Value = serde_json::from_str(&body_of(out).await).unwrap();
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("malformed"));
    }

    #[tokio::test]
    async fn non_2xx_becomes_a_google_error_with_retry_after() {
        let mut response = Response::new(Body::from(
            json!({"type":"error","error":{"type":"rate_limit_error","message":"slow down"}})
                .to_string(),
        ));
        *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, http::HeaderValue::from_static("7"));
        let out = adapt(response, "m".into(), Method::StreamGenerateContent, None).await;
        assert_eq!(out.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(out.headers()[header::RETRY_AFTER], "7");
        let body: Value = serde_json::from_str(&body_of(out).await).unwrap();
        assert_eq!(
            body,
            json!({"error": {"code": 429, "message": "slow down", "status": "RESOURCE_EXHAUSTED"}})
        );
    }

    /// Native gemini frames (already unwrapped by `NativeStream`) are relayed
    /// as they are — signatures included — and aggregate the same way.
    #[tokio::test]
    async fn native_frames_pass_through_and_aggregate() {
        let native = [
            json!({"candidates":[{"content":{"role":"model","parts":[{"text":"pl","thought":true}]},"index":0}],"responseId":"r"}),
            json!({"candidates":[{"content":{"role":"model","parts":[{"text":"an","thought":true}]},"index":0}],"responseId":"r"}),
            json!({"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"f","args":{},"id":"call_1"},"thoughtSignature":"SIG"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":5},"modelVersion":"gemini-3.8-flash-high","responseId":"r"}),
        ];
        let body: String = native
            .iter()
            .map(|v| format!("data: {v}\r\n\r\n"))
            .collect();
        let out = adapt(
            Response::new(Body::from(body.clone())),
            "m".into(),
            Method::StreamGenerateContent,
            None,
        )
        .await;
        assert_eq!(frames(&body_of(out).await), native.to_vec());
        let out = adapt(
            Response::new(Body::from(body)),
            "m".into(),
            Method::GenerateContent,
            None,
        )
        .await;
        let value: Value = serde_json::from_str(&body_of(out).await).unwrap();
        assert_eq!(
            value["candidates"][0]["content"]["parts"],
            json!([
                {"text": "plan", "thought": true},
                {"functionCall": {"name": "f", "args": {}, "id": "call_1"}, "thoughtSignature": "SIG"},
            ])
        );
        assert_eq!(value["modelVersion"], "gemini-3.8-flash-high");
        assert_eq!(value["usageMetadata"]["promptTokenCount"], 5);
    }

    #[test]
    fn native_stream_unwraps_cloud_code_frames_and_reads_usage() {
        let mut stream = NativeStream::default();
        let inner = json!({"candidates":[{"content":{"role":"model","parts":[{"text":"hi","thoughtSignature":"S"}]},"finishReason":"STOP"}],
            "usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":60,"candidatesTokenCount":5,"thoughtsTokenCount":3}});
        let out = stream.on_event(&format!(
            "data: {}",
            json!({"response": inner, "traceId": "t"})
        ));
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("data: {inner}\r\n\r\n")
        );
        assert!(stream.on_end().is_empty(), "finishReason seen");
        let usage = stream.usage();
        assert_eq!(usage.input_tokens, 40);
        assert_eq!(usage.cache_read_input_tokens, Some(60));
        assert_eq!(usage.output_tokens, 8);
        assert_eq!(stream.events_seen(), 1);
        assert!(stream.raw_usage().is_some());

        let mut cut = NativeStream::default();
        cut.on_event(r#"data: {"response":{"candidates":[{"content":{"parts":[{"text":"x"}]}}]}}"#);
        let tail: Value = serde_json::from_str(
            String::from_utf8(cut.on_end())
                .unwrap()
                .trim()
                .strip_prefix("data: ")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(tail["error"]["status"], "UNAVAILABLE");
        assert!(cut.error_message().unwrap().contains("finishReason"));

        let mut failed = NativeStream::default();
        let out = failed.on_event(
            r#"data: {"error":{"code":429,"message":"quota","status":"RESOURCE_EXHAUSTED"}}"#,
        );
        let value: Value = serde_json::from_str(
            String::from_utf8(out)
                .unwrap()
                .trim()
                .strip_prefix("data: ")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["error"]["code"], 429);
        assert!(failed.on_end().is_empty());
    }
}
