//! Google Gemini API ingress (`/v1beta/models/…`, the public `genai` wire)
//! and its return boundary. The Antigravity CLI (`agy` ≥ 1.5.4) speaks this
//! wire in gateway mode (`AGY_LLM_GATEWAY_URL`), so `llmux run --agy` points
//! it here (#316, docs/agy-frontend/README.md).
//!
//! - [`discovery`] — `GET /v1beta/models`: the llmux catalog as the Gemini
//!   model list agy reads its picker, output caps and effort gate from.
//! - [`request`] — the genai body translated to an Anthropic Messages body for
//!   every backend that does not speak it (claude via the Agent SDK, codex,
//!   grok, openrouter).
//! - [`response`] — the shared loop's answer back to the genai wire: Messages
//!   SSE converted, the gemini group's native frames relayed (the
//!   `v1internal` `{"response":…}` wrapper removed), `generateContent`
//!   aggregated, errors in the Google envelope.
//!
//! The gemini group serves this ingress natively: the body goes up in the
//! Cloud Code envelope ([`crate::provider::gemini::GeminiProvider::build_native_request`])
//! with its `thoughtSignature`s intact.

pub(crate) mod discovery;
pub(crate) mod request;
pub(crate) mod response;

use axum::{body::Body, response::Response};
use bytes::Bytes;
use http::{header, StatusCode};
use serde_json::{json, Value};

/// The Gemini API model methods this ingress serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    /// `:streamGenerateContent` (`?alt=sse`) — every agy model turn.
    StreamGenerateContent,
    /// `:generateContent` — one aggregated JSON response.
    GenerateContent,
    /// `:countTokens` — answered locally with an estimate.
    CountTokens,
}

impl Method {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "streamGenerateContent" => Some(Self::StreamGenerateContent),
            "generateContent" => Some(Self::GenerateContent),
            "countTokens" => Some(Self::CountTokens),
            _ => None,
        }
    }

    /// Whether the client reads the answer as an SSE stream.
    pub(crate) fn streams(self) -> bool {
        self == Self::StreamGenerateContent
    }
}

/// Whether `path` belongs to the Gemini API ingress (`/v1beta` and below).
pub(crate) fn is_genai_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    path == "/v1beta" || path.starts_with("/v1beta/") || path.starts_with("/v1beta:")
}

/// `path` without its query and its `/v1beta` prefix. A doubled prefix
/// (`/v1beta/v1beta/models/…`, a gateway URL that already ended in `/v1beta`
/// on an agy that did not normalize it) is tolerated.
fn route(path: &str) -> &str {
    let path = path.split('?').next().unwrap_or(path);
    let rest = path.strip_prefix("/v1beta").unwrap_or(path);
    match rest.strip_prefix("/v1beta") {
        Some(again) if again.is_empty() || again.starts_with('/') => again,
        _ => rest,
    }
}

/// The `(model, method)` a `POST /v1beta/models/{model}:{method}` path names.
/// The model is percent-decoded, and a `models/` resource prefix is stripped
/// (`/v1beta/models/models%2Fx:…`). `Err` is a Google-shaped 404 detail.
pub(crate) fn parse_target(path: &str) -> Result<(String, Method), (StatusCode, String)> {
    let not_found = |detail: String| (StatusCode::NOT_FOUND, detail);
    let rest = route(path);
    let target = rest
        .strip_prefix("/models/")
        .ok_or_else(|| not_found(format!("no Gemini API method is served at {rest}")))?;
    let target = percent_decode(target)
        .ok_or_else(|| not_found("the model in the path is not valid UTF-8".to_string()))?;
    let (model, method) = target
        .rsplit_once(':')
        .ok_or_else(|| not_found(format!("models/{target} names no method")))?;
    let method = Method::parse(method).ok_or_else(|| {
        not_found(format!(
            "method `{method}` is not served (streamGenerateContent, generateContent, countTokens)"
        ))
    })?;
    let model = model.strip_prefix("models/").unwrap_or(model).trim();
    if model.is_empty() {
        return Err(not_found("the path names no model".to_string()));
    }
    Ok((model.to_string(), method))
}

/// `%XX` → byte, then UTF-8. `None` on a malformed escape or non-UTF-8 bytes.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The buffered genai body, validated: a JSON object whose `contents` is an
/// array of `{role?, parts:[…]}` objects. `Err` is a client-facing detail for
/// a local 400.
pub(crate) fn validate(body: &[u8]) -> Result<Value, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| "body must be a JSON object".to_string())?;
    if !value.is_object() {
        return Err("body must be a JSON object".into());
    }
    let contents = value
        .get("contents")
        .and_then(Value::as_array)
        .ok_or("contents must be an array")?;
    if contents.is_empty() {
        return Err("contents must not be empty".into());
    }
    for (index, content) in contents.iter().enumerate() {
        if !content.is_object() {
            return Err(format!("contents[{index}] must be an object"));
        }
        if content
            .get("role")
            .is_some_and(|role| !role.is_null() && !role.is_string())
        {
            return Err(format!("contents[{index}].role must be a string"));
        }
        if !content.get("parts").is_some_and(Value::is_array) {
            return Err(format!("contents[{index}].parts must be an array"));
        }
    }
    for field in ["tools", "safetySettings"] {
        if value
            .get(field)
            .is_some_and(|v| !v.is_null() && !v.is_array())
        {
            return Err(format!("{field} must be an array"));
        }
    }
    for field in ["generationConfig", "toolConfig", "systemInstruction"] {
        if value.get(field).is_some_and(|v| {
            !v.is_null() && !v.is_object() && !(field == "systemInstruction" && v.is_string())
        }) {
            return Err(format!("{field} must be an object"));
        }
    }
    Ok(value)
}

/// The effort a genai body asks for: `generationConfig.thinkingConfig.thinkingLevel`
/// → `minimal|low|medium|high|xhigh|max`, bare (`HIGH`) or with the enum
/// prefix (`THINKING_LEVEL_HIGH`). agy's `ThinkingLevel` enum (1.5.13 binary)
/// is `UNSPECIFIED, MINIMAL, LOW, MEDIUM, HIGH, XHIGH, MAX, MODEL_DEFAULT`;
/// in gateway mode it sent only `LOW|MEDIUM|HIGH` and refused `--effort
/// xhigh|max` client-side (live probe 2026-11-21), so `XHIGH`/`MAX` are
/// mapped for a future build rather than seen. `UNSPECIFIED`, `MODEL_DEFAULT`
/// and a `thinkingBudget` (agy sends 1024 by default) are no effort.
pub(crate) fn effort(body: &Value) -> Option<&'static str> {
    let level = body
        .pointer("/generationConfig/thinkingConfig/thinkingLevel")
        .and_then(Value::as_str)?
        .trim()
        .to_ascii_uppercase();
    match level.strip_prefix("THINKING_LEVEL_").unwrap_or(&level) {
        "MINIMAL" => Some("minimal"),
        "LOW" => Some("low"),
        "MEDIUM" => Some("medium"),
        "HIGH" => Some("high"),
        "XHIGH" | "X_HIGH" => Some("xhigh"),
        "MAX" => Some("max"),
        _ => None,
    }
}

/// The text of the newest `user` content that has any (thought parts left
/// out), for the activity excerpt. agy wraps the typed prompt in
/// `<USER_REQUEST>…</USER_REQUEST>` followed by metadata blocks; only the
/// typed prompt is kept.
fn last_user_text(body: &Value) -> String {
    let text = body["contents"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .filter(|content| content["role"].as_str().unwrap_or("user") == "user")
        .find_map(|content| {
            let text = content["parts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|part| part["thought"] != true)
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        })
        .unwrap_or_default();
    match text
        .split_once("<USER_REQUEST>")
        .and_then(|(_, rest)| rest.split_once("</USER_REQUEST>"))
    {
        Some((inner, _)) => inner.trim().to_string(),
        None => text,
    }
}

/// The Messages-shaped summary the shared routing / activity paths read for
/// a genai request: the routed model, the effort, the last user text. Never
/// forwarded upstream.
pub(crate) fn metadata_request(model: &str, body: &Value) -> Bytes {
    Bytes::from(
        json!({
            "model": model,
            "stream": true,
            "_llmux_native_genai": true,
            "output_config": {"effort": effort(body)},
            "messages": [{"role": "user", "content": last_user_text(body)}],
        })
        .to_string(),
    )
}

/// The model a genai request is routed and served as. agy-facing ids carry
/// no `[1m]` ([`discovery`]), so a discovery id whose catalog row exists ONLY
/// with the suffix (`claude-opus-4-8` → `claude-opus-4-8[1m]`) is served as
/// that row, keeping the context window its discovery entry advertised.
/// Every other id is served as sent.
pub(crate) fn served_model(model: &str) -> String {
    let rows = crate::catalog::catalog("", "", "");
    let has = |id: &str| rows.iter().any(|row| row.id.eq_ignore_ascii_case(id));
    if !has(model) && !model.to_ascii_lowercase().ends_with("[1m]") {
        let suffixed = format!("{model}[1m]");
        if has(&suffixed) {
            return suffixed;
        }
    }
    model.to_string()
}

/// The Google RPC status name for an HTTP status.
pub(crate) fn status_name(status: StatusCode) -> &'static str {
    match status.as_u16() {
        401 => "UNAUTHENTICATED",
        403 => "PERMISSION_DENIED",
        404 => "NOT_FOUND",
        429 => "RESOURCE_EXHAUSTED",
        502..=504 => "UNAVAILABLE",
        400..=499 => "INVALID_ARGUMENT",
        _ => "INTERNAL",
    }
}

/// The Google error object `{"code","message","status"}` for `status`.
pub(crate) fn error_object(status: StatusCode, message: &str) -> Value {
    json!({"code": status.as_u16(), "message": message, "status": status_name(status)})
}

/// A Google-shaped error response: `{"error":{"code","message","status"}}`.
pub(crate) fn error(status: StatusCode, message: &str) -> Response {
    let mut response = Response::new(Body::from(
        json!({"error": error_object(status, message)}).to_string(),
    ));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    response
}

/// `:countTokens`, answered locally: about four bytes of JSON text per token
/// over what the request would send (contents, system instruction, tools).
/// Accepts both the bare `{contents}` body and `{generateContentRequest}`.
pub(crate) fn count_tokens(body: &[u8]) -> Response {
    let value: Value = match serde_json::from_slice(body) {
        Ok(value @ Value::Object(_)) => value,
        _ => return error(StatusCode::BAD_REQUEST, "body must be a JSON object"),
    };
    let request = value.get("generateContentRequest").unwrap_or(&value);
    let bytes: usize = ["contents", "systemInstruction", "tools"]
        .iter()
        .filter_map(|key| request.get(*key))
        .map(|part| part.to_string().len())
        .sum();
    let mut response = Response::new(Body::from(
        json!({"totalTokens": bytes.div_ceil(4)}).to_string(),
    ));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_name_the_model_and_the_method() {
        assert!(is_genai_path("/v1beta/models"));
        assert!(is_genai_path(
            "/v1beta/models/x:streamGenerateContent?alt=sse"
        ));
        assert!(is_genai_path("/v1beta:fetchDefaultConfig"));
        assert!(!is_genai_path("/v1/messages"));
        assert!(!is_genai_path("/v1betax"));

        for (path, model, method) in [
            (
                "/v1beta/models/gpt-6.2-sol:streamGenerateContent?alt=sse",
                "gpt-6.2-sol",
                Method::StreamGenerateContent,
            ),
            (
                "/v1beta/models/claude-opus-5-5:generateContent",
                "claude-opus-5-5",
                Method::GenerateContent,
            ),
            (
                "/v1beta/models/models%2Fgemini-3.8-flash-high:countTokens",
                "gemini-3.8-flash-high",
                Method::CountTokens,
            ),
            (
                "/v1beta/models/claude-opus-4-8%5B1m%5D:streamGenerateContent",
                "claude-opus-4-8[1m]",
                Method::StreamGenerateContent,
            ),
            (
                "/v1beta/v1beta/models/grok-4.7:streamGenerateContent",
                "grok-4.7",
                Method::StreamGenerateContent,
            ),
            (
                "/v1beta/models/or-vendor/slug:generateContent",
                "or-vendor/slug",
                Method::GenerateContent,
            ),
        ] {
            assert_eq!(
                parse_target(path).unwrap(),
                (model.to_string(), method),
                "{path}"
            );
        }
        for path in [
            "/v1beta:fetchDefaultConfig",
            "/v1beta/models/x",
            "/v1beta/models/x:embedContent",
            "/v1beta/models/:generateContent",
            "/v1beta/models/%ZZ:generateContent",
            "/v1beta/cachedContents",
        ] {
            assert_eq!(
                parse_target(path).unwrap_err().0,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
    }

    #[test]
    fn validate_takes_a_genai_body_only() {
        assert!(validate(br#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#).is_ok());
        for (body, detail) in [
            (&b"[]"[..], "body must be a JSON object"),
            (b"{}", "contents must be an array"),
            (br#"{"contents":[]}"#, "contents must not be empty"),
            (br#"{"contents":[1]}"#, "contents[0] must be an object"),
            (
                br#"{"contents":[{"parts":{}}]}"#,
                "contents[0].parts must be an array",
            ),
            (
                br#"{"contents":[{"parts":[]}],"tools":{}}"#,
                "tools must be an array",
            ),
            (
                br#"{"contents":[{"parts":[]}],"generationConfig":[]}"#,
                "generationConfig must be an object",
            ),
        ] {
            assert_eq!(validate(body).unwrap_err(), detail);
        }
    }

    #[test]
    fn metadata_summarizes_model_effort_and_the_typed_prompt() {
        let body = json!({
            "contents": [
                {"role": "user", "parts": [{"text": "<USER_REQUEST>\nfix the bug\n</USER_REQUEST>\n<ADDITIONAL_METADATA>t</ADDITIONAL_METADATA>"}]},
                {"role": "model", "parts": [{"functionCall": {"name": "f", "args": {}}}]},
                {"role": "user", "parts": [{"functionResponse": {"name": "f", "response": {"output": "x"}}}]},
            ],
            "generationConfig": {"thinkingConfig": {"includeThoughts": true, "thinkingLevel": "HIGH"}},
        });
        let summary: Value =
            serde_json::from_slice(&metadata_request("gpt-6.2-sol", &body)).unwrap();
        assert_eq!(summary["model"], "gpt-6.2-sol");
        assert_eq!(summary["output_config"]["effort"], "high");
        assert_eq!(summary["messages"][0]["content"], "fix the bug");
        assert_eq!(summary["_llmux_native_genai"], true);
        let budget = json!({"contents": [], "generationConfig": {"thinkingConfig": {"thinkingBudget": 1024}}});
        assert_eq!(effort(&budget), None);
        for (level, expected) in [
            ("LOW", Some("low")),
            ("medium", Some("medium")),
            ("MINIMAL", Some("minimal")),
            ("XHIGH", Some("xhigh")),
            ("THINKING_LEVEL_XHIGH", Some("xhigh")),
            ("MAX", Some("max")),
            ("THINKING_LEVEL_HIGH", Some("high")),
            ("MODEL_DEFAULT", None),
            ("THINKING_LEVEL_UNSPECIFIED", None),
        ] {
            let body = json!({"generationConfig": {"thinkingConfig": {"thinkingLevel": level}}});
            assert_eq!(effort(&body), expected, "{level}");
        }
    }

    #[test]
    fn served_model_restores_a_suffix_only_catalog_row() {
        assert_eq!(served_model("claude-opus-4-8"), "claude-opus-4-8[1m]");
        assert_eq!(served_model("claude-fable-5-2"), "claude-fable-5-2[1m]");
        assert_eq!(served_model("claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(served_model("gpt-6.2-sol"), "gpt-6.2-sol");
        assert_eq!(served_model("grok-code-fast-9"), "grok-code-fast-9");
        assert_eq!(served_model("claude-opus-4-8[1m]"), "claude-opus-4-8[1m]");
    }

    #[tokio::test]
    async fn errors_use_the_google_envelope() {
        for (status, name) in [
            (StatusCode::BAD_REQUEST, "INVALID_ARGUMENT"),
            (StatusCode::UNAUTHORIZED, "UNAUTHENTICATED"),
            (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
            (StatusCode::NOT_FOUND, "NOT_FOUND"),
            (StatusCode::PAYLOAD_TOO_LARGE, "INVALID_ARGUMENT"),
            (StatusCode::TOO_MANY_REQUESTS, "RESOURCE_EXHAUSTED"),
            (StatusCode::BAD_GATEWAY, "UNAVAILABLE"),
            (StatusCode::SERVICE_UNAVAILABLE, "UNAVAILABLE"),
            (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"),
        ] {
            let response = error(status, "nope");
            assert_eq!(response.status(), status);
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                body,
                json!({"error": {"code": status.as_u16(), "message": "nope", "status": name}})
            );
        }
    }

    #[tokio::test]
    async fn count_tokens_is_a_local_estimate() {
        let response =
            count_tokens(br#"{"contents":[{"role":"user","parts":[{"text":"hello world"}]}]}"#);
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body["totalTokens"].as_u64().unwrap() > 0);
        let nested = count_tokens(
            br#"{"generateContentRequest":{"contents":[{"parts":[{"text":"hello world"}]}]}}"#,
        );
        assert_eq!(nested.status(), StatusCode::OK);
        assert_eq!(count_tokens(b"[]").status(), StatusCode::BAD_REQUEST);
    }
}
