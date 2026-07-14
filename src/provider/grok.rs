//! xAI Grok provider (docs/grok/spec.md): serves Anthropic Messages API
//! requests from a Grok subscription account via xAI's Responses API — the
//! same wire family codex speaks (CLIProxyAPI's xAI thinking applier
//! literally embeds its codex applier), so all translation lives in
//! [`super::responses`]; this module is the thin adapter: model/effort
//! resolution against the grok catalog, auth/identity headers, endpoint.

use bytes::Bytes;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, HeaderValue, Method};
use serde_json::Value;

use super::responses::{self, RequestPlan, ResponsesSseConverter, RESPONSES_PATH};
use super::{ProviderError, ProviderRequest};
use crate::config::AccountCredential;

/// Fallback model slug when none is configured; the configurable default
/// lives in `config.grok.default_model`.
pub const GROK_MODEL: &str = "grok-4.5";

/// Official Grok-CLI chat-proxy base URL (the subscription chat path,
/// CLIProxyAPI `internal/auth/xai/types.go:13`). The identity trio below is
/// attached only when the configured upstream is this host.
pub const GROK_CHAT_PROXY_UPSTREAM: &str = "https://cli-chat-proxy.grok.com/v1";

/// Grok-CLI identity headers the official cli-chat-proxy expects
/// (CLIProxyAPI xai_executor.go:66-69). The client version ages with the
/// Grok CLI; bump when upstream starts rejecting it.
const GROK_TOKEN_AUTH_HEADER: &str = "x-xai-token-auth";
const GROK_TOKEN_AUTH_VALUE: &str = "xai-grok-cli";
const GROK_CLIENT_VERSION_HEADER: &str = "x-grok-client-version";
const GROK_CLIENT_VERSION_VALUE: &str = "0.2.93";

/// Per-model thinking levels (docs/grok/spec.md §R1; source: CLIProxyAPI
/// registry models.json:2411-2520). Models NOT listed here get no
/// `reasoning` field at all — omission is the only universally-accepted
/// wire form (e.g. `grok-build-0.1` has no thinking support).
const GROK_THINKING_LEVELS: &[(&str, &[&str])] = &[
    ("grok-4.5", &["low", "medium", "high"]),
    ("grok-4.3", &["none", "low", "medium", "high"]),
    ("grok-3-mini", &["low", "medium", "high"]),
];

/// Efforts a client/config may name; anything else is ignored (shape
/// fallback). Superset across providers so Claude Agent SDK values
/// (`output_config.effort`) map cleanly (codex parity).
const GROK_EFFORT_INPUTS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

/// Request-shaping knobs for the grok Responses request, sourced from
/// `config.grok`. No `fast` — xAI has no service tier.
#[derive(Debug, Clone)]
pub struct GrokShape {
    /// Model slug requested upstream when the client's model is not
    /// grok-shaped.
    pub model: String,
    /// When `Some`, the model NAME reported to the client (Claude Code) in
    /// the synthesized Anthropic response (same contract as
    /// `codex.client_model`).
    pub client_model: Option<String>,
    /// Configured `reasoning.effort` default (superset `none|low|medium|high`;
    /// clamped per-model at request time), or `None` for the backend default.
    pub effort: Option<String>,
}

impl Default for GrokShape {
    fn default() -> Self {
        Self {
            model: GROK_MODEL.to_string(),
            client_model: None,
            effort: None,
        }
    }
}

impl GrokShape {
    /// Build from the on-disk grok config.
    pub fn from_config(grok: &crate::config::schema::GrokConfig) -> Self {
        Self {
            model: grok.default_model.clone(),
            client_model: grok.client_model.clone(),
            effort: grok.reasoning_effort.clone(),
        }
    }
}

/// The grok provider: upstream base URL, live-mutable request shape
/// (model/effort — `POST /llmux/grok`), and a per-process session id sent as
/// `prompt_cache_key` (cache hint only; NO `x-grok-conv-id` header — spec
/// §R1, CLIProxyAPI omits it for standard chat).
#[derive(Debug)]
pub struct GrokProvider {
    base_url: String,
    shape: std::sync::RwLock<GrokShape>,
    session_id: String,
}

impl GrokProvider {
    /// Construct with the default request shape (pinned `grok-4.5`).
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_shape(base_url, GrokShape::default())
    }

    /// Construct with an explicit request shape (from `config.grok`).
    pub fn with_shape(base_url: impl Into<String>, shape: GrokShape) -> Self {
        Self {
            base_url: base_url.into(),
            shape: std::sync::RwLock::new(shape),
            session_id: responses::uuid_v4(),
        }
    }

    /// Snapshot the current request shape.
    pub fn shape(&self) -> GrokShape {
        self.shape.read().expect("grok shape lock").clone()
    }

    /// Replace the live request shape (`POST /llmux/grok`).
    pub fn set_shape(&self, shape: GrokShape) {
        *self.shape.write().expect("grok shape lock") = shape;
    }

    /// The model slug this provider currently requests (for the activity log).
    pub fn model(&self) -> String {
        self.shape.read().expect("grok shape lock").model.clone()
    }

    /// The reasoning effort this provider currently sends (activity log).
    pub fn effort(&self) -> Option<String> {
        self.shape.read().expect("grok shape lock").effort.clone()
    }

    /// The PER-REQUEST effective `(upstream model, reasoning effort)` for
    /// `anthropic_body` under the live shape — the exact values
    /// [`Self::build_request`] would send upstream, for the activity log.
    pub fn request_meta(&self, anthropic_body: &[u8]) -> (String, Option<String>) {
        let shape = self.shape();
        let body = serde_json::from_slice::<Value>(anthropic_body).unwrap_or(Value::Null);
        effective_request_meta(&body, &shape)
    }

    pub fn endpoint(&self) -> &str {
        &self.base_url
    }

    /// Whether the configured upstream is the official Grok-CLI chat proxy
    /// (identity headers attach only then — spec §R1 / C3).
    fn is_official_chat_proxy(&self) -> bool {
        normalize_base_url(&self.base_url) == normalize_base_url(GROK_CHAT_PROXY_UPSTREAM)
    }

    /// Build the upstream Responses request from an Anthropic Messages body:
    /// translate via the shared core, set the grok header set, inject the
    /// credential. Returns the request plus whether the CLIENT asked for
    /// streaming (upstream is always `stream: true`).
    pub fn build_request(
        &self,
        anthropic_body: &[u8],
        credential: &AccountCredential,
    ) -> Result<(ProviderRequest, bool), ProviderError> {
        let AccountCredential::Grok { access_token, .. } = credential else {
            return Err(ProviderError::Auth(
                "grok provider requires a grok credential".into(),
            ));
        };
        let body: Value = serde_json::from_slice(anthropic_body)
            .map_err(|err| ProviderError::Convert(format!("request body is not JSON: {err}")))?;
        let (upstream_body, client_stream) =
            translate_request_with(&body, &self.session_id, &self.shape())?;

        let mut headers = HeaderMap::new();
        let bearer = HeaderValue::from_str(&format!("Bearer {access_token}"))
            .map_err(|err| ProviderError::Auth(err.to_string()))?;
        headers.insert(AUTHORIZATION, bearer);
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if self.is_official_chat_proxy() {
            headers.insert(
                GROK_TOKEN_AUTH_HEADER,
                HeaderValue::from_static(GROK_TOKEN_AUTH_VALUE),
            );
            headers.insert(
                GROK_CLIENT_VERSION_HEADER,
                HeaderValue::from_static(GROK_CLIENT_VERSION_VALUE),
            );
            headers.insert(
                http::header::USER_AGENT,
                HeaderValue::from_static(
                    concat!("xai-grok-workspace/", "0.2.93"), // keep in sync with GROK_CLIENT_VERSION_VALUE
                ),
            );
        }

        Ok((
            ProviderRequest {
                method: Method::POST,
                path: RESPONSES_PATH.to_string(),
                headers,
                body: Bytes::from(upstream_body.to_string()),
            },
            client_stream,
        ))
    }

    /// Fresh per-request stream converter, stamping responses with this
    /// provider's configured model slug — or the `client_model` override.
    pub fn converter(&self) -> ResponsesSseConverter {
        let shape = self.shape();
        ResponsesSseConverter::with_model(shape.model)
            .with_client_model(shape.client_model)
            .with_tag("grok")
    }
}

/// Trailing-slash-insensitive, case-insensitive base-URL comparison key.
fn normalize_base_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_ascii_lowercase()
}

/// Resolve the model slug requested upstream: grok-shaped requests
/// (`grok-` prefix / bare `grok`) pass through VERBATIM — the client's
/// choice is honored, `/model grok-4.5` from Claude Code works with no
/// config change (spec §R4); everything else (Anthropic default models on
/// fallback, model-less requests) keeps the configured pin.
fn resolve_upstream_model(requested: Option<&str>, pinned: &str) -> String {
    let Some(req) = requested else {
        return pinned.to_string();
    };
    let req = req.trim().to_ascii_lowercase();
    if req == "grok" {
        // Bare family alias → the configured pin (routing classifies it
        // here; there is no upstream model literally named "grok").
        return pinned.to_string();
    }
    if req.starts_with("grok-") {
        return req;
    }
    pinned.to_string()
}

/// The per-model thinking-level table, for the model catalog
/// (`src/catalog.rs`). Exposes [`GROK_THINKING_LEVELS`] without duplicating
/// the effort lists there.
pub(crate) fn thinking_levels_catalog() -> &'static [(&'static str, &'static [&'static str])] {
    GROK_THINKING_LEVELS
}

/// Thinking levels for `model`, when it is a known reasoning model.
fn thinking_levels(model: &str) -> Option<&'static [&'static str]> {
    let m = model.to_ascii_lowercase();
    GROK_THINKING_LEVELS
        .iter()
        .find(|(id, _)| *id == m)
        .map(|(_, levels)| *levels)
}

/// Per-request reasoning effort for grok (spec §R1, single-source rule):
/// the request's `output_config.effort` (Claude Agent SDK wire) wins over
/// the configured shape effort; the winner clamps INTO the effective
/// model's level set. Models outside [`GROK_THINKING_LEVELS`] always yield
/// `None` (omit `reasoning`). A clamped result of `none` also yields `None`
/// — omission is the only universally-accepted zero form.
fn resolve_reasoning_effort(
    body: &Value,
    shape_effort: Option<&str>,
    upstream_model: &str,
) -> Option<String> {
    let levels = thinking_levels(upstream_model)?;
    let requested = body
        .get("output_config")
        .and_then(|c| c.get("effort"))
        .and_then(Value::as_str)
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| GROK_EFFORT_INPUTS.contains(&e.as_str()));
    let configured = shape_effort
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty() && e != "default")
        .filter(|e| GROK_EFFORT_INPUTS.contains(&e.as_str()));
    let candidate = requested.or(configured)?;
    let clamped = match candidate.as_str() {
        "none" | "minimal" => {
            if levels.contains(&"none") {
                return None; // zero allowed → express as omission
            }
            "low"
        }
        "xhigh" | "max" | "ultra" => "high",
        other => other,
    };
    if levels.contains(&clamped) {
        Some(clamped.to_string())
    } else {
        // A level set that lacks the clamped value (future model rows) —
        // omit rather than guess.
        None
    }
}

/// The PER-REQUEST effective `(upstream model, reasoning effort)` this body
/// would send upstream under `shape` — the single source the activity log
/// reads (codex parity: `effective_request_meta`).
pub fn effective_request_meta(body: &Value, shape: &GrokShape) -> (String, Option<String>) {
    let requested_model = body.get("model").and_then(Value::as_str);
    let upstream_model = resolve_upstream_model(requested_model, &shape.model);
    let effort = resolve_reasoning_effort(body, shape.effort.as_deref(), &upstream_model);
    (upstream_model, effort)
}

/// Translate an Anthropic Messages body into the grok Responses body under
/// `shape`: grok-shaped requested slugs pass through verbatim, effort
/// resolves per-model, and the shared core does the rest. NO
/// `include: [reasoning.encrypted_content]` (OpenAI-specific) and NO
/// `service_tier` (xAI has no tier) — C1.
pub fn translate_request_with(
    body: &Value,
    session_id: &str,
    shape: &GrokShape,
) -> Result<(Value, bool), ProviderError> {
    let requested_model = body.get("model").and_then(Value::as_str);
    let upstream_model = resolve_upstream_model(requested_model, &shape.model);
    if let Some(model) = requested_model {
        if model != upstream_model {
            tracing::debug!(
                client_model = model,
                "grok: model rewritten to {}",
                upstream_model
            );
        }
    }
    let effort = resolve_reasoning_effort(body, shape.effort.as_deref(), &upstream_model);
    responses::build_responses_body(
        body,
        &RequestPlan {
            upstream_model: &upstream_model,
            effort,
            priority_tier: false,
            include_encrypted_reasoning: false,
            session_id,
        },
    )
}

/// Valid values for `POST /llmux/grok`'s `reasoning_effort` (superset —
/// per-model clamping happens at request time, spec §R1). Empty / `unset`
/// clears and is handled by the endpoint before this check.
pub fn is_valid_config_effort(effort: &str) -> bool {
    matches!(effort, "none" | "low" | "medium" | "high")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(model: &str) -> Value {
        json!({
            "model": model,
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}],
        })
    }

    fn shape(model: &str, effort: Option<&str>) -> GrokShape {
        GrokShape {
            model: model.to_string(),
            client_model: None,
            effort: effort.map(str::to_string),
        }
    }

    // ---- C1: verbatim grok model pass-through, clamped effort, no
    // include/service_tier, store:false ----
    #[test]
    fn c1_grok_model_passthrough_with_clamped_effort() {
        let mut b = body("grok-4.5");
        b["output_config"] = json!({"effort": "xhigh"});
        let (upstream, stream) =
            translate_request_with(&b, "sess", &shape("grok-4.5", None)).expect("translate");
        assert!(stream);
        assert_eq!(upstream["model"], "grok-4.5");
        assert_eq!(
            upstream["reasoning"]["effort"], "high",
            "xhigh clamps to high"
        );
        assert!(
            upstream.get("include").is_none(),
            "no encrypted-reasoning include"
        );
        assert!(upstream.get("service_tier").is_none(), "no service tier");
        assert_eq!(upstream["store"], false);
        assert_eq!(upstream["prompt_cache_key"], "sess");
    }

    #[test]
    fn c1_non_grok_model_uses_pin() {
        let (upstream, _) =
            translate_request_with(&body("claude-sonnet-5"), "s", &shape("grok-4.5", None))
                .expect("translate");
        assert_eq!(upstream["model"], "grok-4.5");
    }

    #[test]
    fn c1_other_grok_slug_passes_verbatim() {
        let (upstream, _) =
            translate_request_with(&body("grok-build-0.1"), "s", &shape("grok-4.5", None))
                .expect("translate");
        assert_eq!(upstream["model"], "grok-build-0.1");
    }

    // ---- C4: effort clamping table ----
    #[test]
    fn c4_none_clamps_to_low_when_zero_disallowed() {
        let mut b = body("grok-4.5");
        b["output_config"] = json!({"effort": "none"});
        let (upstream, _) =
            translate_request_with(&b, "s", &shape("grok-4.5", None)).expect("translate");
        assert_eq!(upstream["reasoning"]["effort"], "low");
    }

    #[test]
    fn c4_none_omits_reasoning_when_model_allows_zero() {
        let mut b = body("grok-4.3");
        b["output_config"] = json!({"effort": "none"});
        let (upstream, _) =
            translate_request_with(&b, "s", &shape("grok-4.5", None)).expect("translate");
        assert_eq!(upstream["model"], "grok-4.3");
        assert!(
            upstream.get("reasoning").is_none(),
            "none on grok-4.3 = omission"
        );
    }

    #[test]
    fn c4_non_thinking_model_never_gets_reasoning() {
        let (upstream, _) = translate_request_with(
            &body("grok-build-0.1"),
            "s",
            &shape("grok-4.5", Some("high")),
        )
        .expect("translate");
        assert!(upstream.get("reasoning").is_none());
    }

    #[test]
    fn c4_invalid_request_effort_falls_back_to_shape() {
        let mut b = body("grok-4.5");
        b["output_config"] = json!({"effort": "turbo"});
        let (upstream, _) =
            translate_request_with(&b, "s", &shape("grok-4.5", Some("medium"))).expect("translate");
        assert_eq!(upstream["reasoning"]["effort"], "medium");
    }

    #[test]
    fn c4_no_effort_anywhere_omits_reasoning() {
        let (upstream, _) =
            translate_request_with(&body("grok-4.5"), "s", &shape("grok-4.5", None))
                .expect("translate");
        assert!(
            upstream.get("reasoning").is_none(),
            "backend default (high) applies"
        );
    }

    // ---- C3: headers ----
    #[test]
    fn c3_official_upstream_gets_identity_trio_and_no_conv_id() {
        let provider = GrokProvider::new(GROK_CHAT_PROXY_UPSTREAM);
        let credential = AccountCredential::Grok {
            subject: "sub1".into(),
            access_token: "at-1".into(),
            refresh_token: "rt-1".into(),
            expires_at_ms: 0,
            token_endpoint: String::new(),
            last_refresh_ms: None,
        };
        let (req, _) = provider
            .build_request(body("grok-4.5").to_string().as_bytes(), &credential)
            .expect("build");
        assert_eq!(req.headers.get("authorization").unwrap(), "Bearer at-1");
        assert_eq!(req.headers.get("x-xai-token-auth").unwrap(), "xai-grok-cli");
        assert_eq!(req.headers.get("x-grok-client-version").unwrap(), "0.2.93");
        assert_eq!(
            req.headers.get("user-agent").unwrap(),
            "xai-grok-workspace/0.2.93"
        );
        assert!(
            req.headers.get("x-grok-conv-id").is_none(),
            "no conv id header"
        );
        assert_eq!(req.path, "/responses");
    }

    #[test]
    fn c3_custom_upstream_omits_identity_trio() {
        let provider = GrokProvider::new("https://example.com/v1");
        let credential = AccountCredential::Grok {
            subject: String::new(),
            access_token: "at-2".into(),
            refresh_token: "rt-2".into(),
            expires_at_ms: 0,
            token_endpoint: String::new(),
            last_refresh_ms: None,
        };
        let (req, _) = provider
            .build_request(body("grok-4.5").to_string().as_bytes(), &credential)
            .expect("build");
        assert!(req.headers.get("x-xai-token-auth").is_none());
        assert!(req.headers.get("x-grok-client-version").is_none());
        assert!(req.headers.get("x-grok-conv-id").is_none());
    }

    #[test]
    fn build_request_rejects_non_grok_credential() {
        let provider = GrokProvider::new(GROK_CHAT_PROXY_UPSTREAM);
        let credential = AccountCredential::Apikey {
            api_key: "sk-x".into(),
        };
        assert!(provider
            .build_request(body("grok-4.5").to_string().as_bytes(), &credential)
            .is_err());
    }

    // ---- C16: flavor-parameterized translation through the shared core ----
    #[test]
    fn c16_system_folding_tools_and_tool_round_trip_under_grok_flavor() {
        let b = json!({
            "model": "grok-4.5",
            "stream": true,
            "system": "be terse",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "system", "content": "operator note"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "calling"},
                    {"type": "tool_use", "id": "call_1", "name": "get_x", "input": {"a": 1}},
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "42"},
                ]},
            ],
            "tools": [{"name": "get_x", "description": "d", "input_schema": {"type": "object"}}],
        });
        let (upstream, _) =
            translate_request_with(&b, "s", &shape("grok-4.5", None)).expect("translate");
        assert_eq!(upstream["instructions"], "be terse\noperator note");
        let input = upstream["input"].as_array().unwrap();
        assert!(input
            .iter()
            .any(|i| i["type"] == "function_call" && i["call_id"] == "call_1"));
        assert!(input
            .iter()
            .any(|i| i["type"] == "function_call_output" && i["output"] == "42"));
        assert!(!input.iter().any(|i| i["role"] == "system"));
        let tools = upstream["tools"].as_array().unwrap();
        assert_eq!(tools[0]["name"], "get_x");
        assert_eq!(tools[0]["type"], "function");
    }

    #[test]
    fn c16_grok_converter_stamps_grok_message_id_and_usage() {
        use crate::proxy::sse::SseTransform;
        let mut converter =
            ResponsesSseConverter::with_model("grok-4.5".to_string()).with_tag("grok");
        let mut out = Vec::new();
        out.extend(converter.on_event(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"\",\"model\":\"grok-4.5\"}}",
        ));
        out.extend(
            converter
                .on_event("data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}"),
        );
        out.extend(converter.on_event(
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":4},\"output_tokens\":3}}}",
        ));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("message_start"));
        assert!(text.contains("msg_grok_"), "grok-tagged message id");
        assert!(
            text.contains("\"input_tokens\":6"),
            "fresh = total - cached"
        );
        assert!(text.contains("\"cache_read_input_tokens\":4"));
        assert!(text.contains("message_stop"));
    }

    // ---- C15: non-stream aggregate under grok flavor ----
    #[test]
    fn c15_non_stream_aggregates_to_single_json() {
        use crate::proxy::sse::SseTransform;
        let b = json!({
            "model": "grok-4.5",
            "stream": false,
            "messages": [{"role": "user", "content": "hi"}],
        });
        let (_, client_stream) =
            translate_request_with(&b, "s", &shape("grok-4.5", None)).expect("translate");
        assert!(!client_stream, "client did not ask for SSE");
        let mut converter =
            ResponsesSseConverter::with_model("grok-4.5".to_string()).with_tag("grok");
        let _ = converter.on_event(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"grok-4.5\"}}",
        );
        let _ = converter
            .on_event("data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi there\"}");
        let _ = converter.on_event(
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}",
        );
        let message = converter.into_message_json().expect("aggregate");
        assert_eq!(message["model"], "grok-4.5");
        assert_eq!(message["content"][0]["text"], "hi there");
        assert_eq!(message["usage"]["output_tokens"], 2);
    }

    // ---- resolve_upstream_model unit coverage ----
    #[test]
    fn bare_grok_alias_maps_to_pin() {
        assert_eq!(resolve_upstream_model(Some("grok"), "grok-4.5"), "grok-4.5");
        assert_eq!(
            resolve_upstream_model(Some("GROK-4.3"), "grok-4.5"),
            "grok-4.3"
        );
        assert_eq!(resolve_upstream_model(None, "grok-4.5"), "grok-4.5");
    }

    #[test]
    fn config_effort_validation_superset() {
        for ok in ["none", "low", "medium", "high"] {
            assert!(is_valid_config_effort(ok));
        }
        for bad in ["turbo", "xhigh", "max", "ultra", "minimal"] {
            assert!(!is_valid_config_effort(bad), "{bad} rejected at config");
        }
    }
}
