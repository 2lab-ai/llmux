//! OpenAI Codex provider (minimum viable, Phase C): serves Anthropic
//! Messages API requests from a ChatGPT-subscription Codex account by
//! translating to the Responses API (`POST {base}/responses`, model pinned
//! to [`CODEX_MODEL`]) and converting the Responses SSE stream back into
//! Anthropic SSE on the fly.
//!
//! Translation logic ported from CLIProxyAPI's `codex_claude_request.go` /
//! `codex_claude_response.go` shapes. The Anthropic passthrough never touches
//! this module — its byte-identity path is unchanged.

use bytes::Bytes;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, HeaderValue, Method};
use serde_json::Value;

use super::responses::{self, RequestPlan};
use super::{ProviderError, ProviderRequest};
use crate::config::AccountCredential;

// The shared Responses machinery moved to `super::responses` (R5,
// docs/grok/spec.md); these re-exports keep this module's public surface —
// and its test suite — source-identical.
pub use super::responses::{
    estimate_input_tokens, estimate_section_tokens, ResponsesSseConverter as CodexSseConverter,
    RESPONSES_PATH,
};

/// Fallback model slug when none is configured. The configurable default now
/// lives in `config.codex.default_model`; this const is the compile-time
/// fallback and the value [`CodexShape::default`] uses (so tests and the
/// `new(base_url)` constructor preserve the original pinned behavior).
///
/// `gpt-5.6-sol` (2026-07-09 launch, flagship tier): probed against the
/// ChatGPT-account codex backend — bare `gpt-5.6` and `gpt-5.6-codex` are
/// rejected ("model is not supported when using Codex with a ChatGPT
/// account"); `gpt-5.6-sol` / `gpt-5.6-terra` are accepted. Context window
/// 372,000 per the openai/codex model catalog (probe-consistent: 369,755
/// tokens pass, ~380k rejected; gpt-5.5 is 272k).
pub const CODEX_MODEL: &str = "gpt-5.6-sol";

/// Request-shaping knobs for the Responses request, sourced from
/// `config.codex`. They mirror exactly what the codex CLI sets on the wire:
/// the model slug, `service_tier: "priority"` when `fast`, and a
/// `reasoning.effort` value. [`Default`] reproduces the original behavior
/// (pinned `gpt-5.5`, no fast tier, backend-default effort) so existing tests
/// and the bare `new` constructor are unaffected.
#[derive(Debug, Clone)]
pub struct CodexShape {
    /// Model slug requested upstream.
    pub model: String,
    /// When `Some`, the model NAME reported to the client (Claude Code) in the
    /// synthesized Anthropic response — independent of `model`, which is what
    /// is actually requested upstream and what routing/dashboard/trace use.
    /// `None` (default) → report the real `model`. See
    /// [`crate::config::schema::CodexConfig::client_model`].
    pub client_model: Option<String>,
    /// `true` → send `service_tier: "priority"` (codex "fast" mode).
    pub fast: bool,
    /// `reasoning.effort` value (`none|minimal|low|medium|high|xhigh`), or
    /// `None` to omit the field and let the backend choose.
    pub effort: Option<String>,
}

impl Default for CodexShape {
    fn default() -> Self {
        Self {
            model: CODEX_MODEL.to_string(),
            client_model: None,
            fast: false,
            effort: None,
        }
    }
}

impl CodexShape {
    /// Build from the on-disk codex config.
    pub fn from_config(codex: &crate::config::schema::CodexConfig) -> Self {
        Self {
            model: codex.default_model.clone(),
            client_model: codex.client_model.clone(),
            fast: codex.fast,
            effort: codex.reasoning_effort.clone(),
        }
    }
}

/// The codex provider: holds the upstream base URL, the (live-mutable) request
/// shape (model/fast/effort), and a per-process session id (sent as
/// `session-id` and `prompt_cache_key`, stable so the backend's prompt cache
/// keys stay warm across requests).
///
/// The shape is behind an `RwLock` so the dashboard can toggle fast/model/
/// effort on a running daemon (req8.1) without a restart: requests take a
/// read lock (uncontended on the hot path), the control endpoint takes a write
/// lock.
#[derive(Debug)]
pub struct CodexProvider {
    base_url: String,
    shape: std::sync::RwLock<CodexShape>,
    session_id: String,
}

impl CodexProvider {
    /// Construct with the default request shape (pinned `gpt-5.5`). Used by
    /// tests; production uses [`CodexProvider::with_shape`].
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_shape(base_url, CodexShape::default())
    }

    /// Construct with an explicit request shape (from `config.codex`).
    pub fn with_shape(base_url: impl Into<String>, shape: CodexShape) -> Self {
        Self {
            base_url: base_url.into(),
            shape: std::sync::RwLock::new(shape),
            session_id: responses::uuid_v4(),
        }
    }

    /// Snapshot the current request shape.
    pub fn shape(&self) -> CodexShape {
        self.shape.read().expect("codex shape lock").clone()
    }

    /// Replace the live request shape (dashboard fast/model/effort change).
    pub fn set_shape(&self, shape: CodexShape) {
        *self.shape.write().expect("codex shape lock") = shape;
    }

    /// The model slug this provider currently requests (for the activity log).
    pub fn model(&self) -> String {
        self.shape.read().expect("codex shape lock").model.clone()
    }

    /// The reasoning effort this provider currently sends (for the activity log).
    pub fn effort(&self) -> Option<String> {
        self.shape.read().expect("codex shape lock").effort.clone()
    }

    /// The PER-REQUEST effective `(upstream model, reasoning effort, fast)` for
    /// `anthropic_body` under the live shape — the exact values
    /// [`Self::build_request`] would send upstream, for the activity log.
    /// Reuses [`effective_request_meta`] (no duplicated resolution). A non-JSON
    /// body (which would fail `build_request` anyway) falls back to the shape's
    /// own model/effort/fast.
    pub fn request_meta(&self, anthropic_body: &[u8]) -> (String, Option<String>, bool) {
        let shape = self.shape();
        let body = serde_json::from_slice::<Value>(anthropic_body).unwrap_or(Value::Null);
        effective_request_meta(&body, &shape)
    }

    pub fn endpoint(&self) -> &str {
        &self.base_url
    }

    /// Build the upstream Responses request from an Anthropic Messages body:
    /// translate the body, set the codex header set, inject the credential.
    /// Returns the request plus whether the CLIENT asked for streaming
    /// (upstream is always `stream: true`; non-stream clients get the
    /// aggregated result).
    pub fn build_request(
        &self,
        anthropic_body: &[u8],
        credential: &AccountCredential,
    ) -> Result<(ProviderRequest, bool), ProviderError> {
        let AccountCredential::Codex {
            account_id,
            access_token,
            ..
        } = credential
        else {
            return Err(ProviderError::Auth(
                "codex provider requires a codex credential".into(),
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
        headers.insert(
            "chatgpt-account-id",
            HeaderValue::from_str(account_id)
                .map_err(|err| ProviderError::Auth(err.to_string()))?,
        );
        headers.insert(
            "openai-beta",
            HeaderValue::from_static("responses=experimental"),
        );
        headers.insert("originator", HeaderValue::from_static("codex_cli_rs"));
        headers.insert(
            "session-id",
            HeaderValue::from_str(&self.session_id)
                .map_err(|err| ProviderError::Convert(err.to_string()))?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

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
    /// provider's configured model slug — or the `client_model` override when
    /// set (what Claude Code sees; routing/dashboard/trace keep the real model).
    pub fn converter(&self) -> CodexSseConverter {
        let shape = self.shape();
        CodexSseConverter::with_model(shape.model).with_client_model(shape.client_model)
    }
}

// ---------------------------------------------------------------------------
// Request translation: Anthropic Messages → Responses API
// ---------------------------------------------------------------------------

/// Translate an Anthropic Messages request body into a Responses API body.
/// Returns `(upstream_body, client_requested_stream)`. The model is ALWAYS
/// rewritten to [`CODEX_MODEL`]; `max_tokens` and `tool_choice` are ignored
/// (logged at debug); images and thinking blocks are dropped (warn/debug).
pub fn translate_request(body: &Value, session_id: &str) -> Result<(Value, bool), ProviderError> {
    translate_request_with(body, session_id, &CodexShape::default())
}

/// Upstream slugs the ChatGPT-account codex backend is known to accept
/// (probed 2026-07-10; `gpt-5.6-luna` parses upstream but currently returns
/// "Model not found" — kept so it starts working the moment OpenAI enables
/// it). Requests naming one of these are forwarded VERBATIM; the bare
/// `gpt-5.6` id maps to the sol flagship (the backend rejects the bare id);
/// any other requested model keeps the configured pin.
const PASSTHROUGH_MODELS: &[&str] = &[
    "gpt-5.5",
    "gpt-5.5-codex",
    "gpt-5-codex",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
];

/// The latest gpt generation the bare variant aliases (`sol`/`terra`/`luna`)
/// and the bare `gpt-5.6` id resolve to. This is the ONE const to bump when a
/// new generation ships (e.g. `gpt-5.7`): the aliases then follow automatically.
const LATEST_GPT_GENERATION: &str = "gpt-5.6";

/// Bare variant aliases: routing classifies these to codex (see
/// [`crate::routing`]) and this provider resolves each to the latest gpt
/// generation of that variant (`sol` → `gpt-5.6-sol`, …).
const VARIANT_ALIASES: &[&str] = &["sol", "terra", "luna"];

/// Resolve the model slug requested upstream: the bare variant aliases
/// (`sol`/`terra`/`luna`) and bare `gpt-5.6` map to the latest gpt generation
/// of that variant ([`LATEST_GPT_GENERATION`]); known-valid slugs pass through
/// (the client's choice is honored — soma-work exposes sol/terra as distinct
/// user-selectable models); everything else (unknown ids, model-less requests)
/// keeps the configured pin.
fn resolve_upstream_model(requested: Option<&str>, pinned: &str) -> String {
    let Some(req) = requested else {
        return pinned.to_string();
    };
    let req = req.trim().to_ascii_lowercase();
    // Bare variant alias → latest gpt generation of that variant.
    if VARIANT_ALIASES.contains(&req.as_str()) {
        return format!("{LATEST_GPT_GENERATION}-{req}");
    }
    // The bare generation id is rejected upstream — map it to the sol flagship.
    if req == LATEST_GPT_GENERATION {
        return format!("{LATEST_GPT_GENERATION}-sol");
    }
    if PASSTHROUGH_MODELS.contains(&req.as_str()) {
        return req;
    }
    pinned.to_string()
}

/// Valid codex `reasoning.effort` values. Per the openai/codex model
/// catalog (models-manager/models.json): the gpt-5.6 family supports
/// low/medium/high/xhigh/max (+ `ultra` on sol/terra); gpt-5.5 tops out at
/// xhigh. none/minimal are documented CLI values.
const CODEX_EFFORT_VALUES: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

/// Returns true when `model` belongs to the gpt-5.6 family, which natively
/// supports the `max` / `ultra` reasoning levels (openai/codex models.json).
/// The boundary is exact-generation: `gpt-5.6` itself or a `gpt-5.6-` variant.
/// A bare `starts_with("gpt-5.6")` would wrongly match a hypothetical future
/// `gpt-5.60-...` id, whose effort support is unknown.
fn supports_extended_efforts(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m == "gpt-5.6" || m.starts_with("gpt-5.6-")
}

/// Per-request reasoning effort: a CONFIGURED shape effort (dashboard `e`
/// cycle / config `reasoning_effort`) OVERRIDES whatever the client sent —
/// that is what selecting a concrete value in the UI means (UI-3 U12).
/// Unset / "default" shape = BYPASS: the request's `output_config.effort`
/// (the Claude Agent SDK wire form, observed 2026-07-10) passes through.
/// `max`/`ultra` pass for the gpt-5.6 family (natively supported per the
/// codex model catalog) and clamp to `xhigh` for older models.
///
/// Precedence flipped 2026-07-15 (was request-wins): with request-wins a
/// configured effort was dead weight for Claude Code traffic, which always
/// sends `output_config.effort` — the operator's pick could never apply.
fn resolve_reasoning_effort(
    body: &Value,
    shape_effort: Option<&str>,
    upstream_model: &str,
) -> Option<String> {
    let clamp = |e: String| {
        if (e == "max" || e == "ultra") && !supports_extended_efforts(upstream_model) {
            "xhigh".to_string()
        } else {
            e
        }
    };
    let configured = shape_effort.and_then(|e| {
        let e = e.trim();
        if e.is_empty() || e.eq_ignore_ascii_case("default") {
            None // bypass — the client's value rides through below
        } else {
            Some(clamp(e.to_ascii_lowercase()))
        }
    });
    configured.or_else(|| {
        body.get("output_config")
            .and_then(|c| c.get("effort"))
            .and_then(Value::as_str)
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| CODEX_EFFORT_VALUES.contains(&e.as_str()))
            .map(clamp)
    })
}

/// The PER-REQUEST effective `(upstream model, reasoning effort, fast)` this
/// body would send upstream under `shape`, WITHOUT building the whole request —
/// the single source the activity log reads so its recorded model/effort/fast
/// equal what actually went on the wire. Mirrors [`translate_request_with`]
/// exactly: the model is the [`resolve_upstream_model`] result (a requested
/// `gpt-5.5` is recorded as `gpt-5.5` even when the shape pins `gpt-5.6-sol`,
/// and a FAILED request still names the model that failed); effort resolves
/// against that resolved model (so a `max` on an older model is recorded as the
/// clamped `xhigh`); `fast` is the shape's fast flag. Returning the tuple here
/// avoids duplicating the resolution logic in the proxy forward path.
pub fn effective_request_meta(body: &Value, shape: &CodexShape) -> (String, Option<String>, bool) {
    let requested_model = body.get("model").and_then(Value::as_str);
    let upstream_model = resolve_upstream_model(requested_model, &shape.model);
    let effort = resolve_reasoning_effort(body, shape.effort.as_deref(), &upstream_model);
    (upstream_model, effort, shape.fast)
}

/// Like [`translate_request`] but with an explicit request [`CodexShape`]
/// (configurable model / fast tier / reasoning effort). Known-valid slugs in
/// the request pass through verbatim (`resolve_upstream_model`); everything
/// else is rewritten to `shape.model`. `max_tokens` and `tool_choice` are
/// ignored (logged at debug); images and thinking blocks are dropped
/// (warn/debug). When `shape.fast`, `service_tier: "priority"` is added (the
/// wire value the codex CLI sends for fast mode); reasoning effort comes from
/// the request's `output_config.effort` when valid, else `shape.effort`
/// (`resolve_reasoning_effort`).
pub fn translate_request_with(
    body: &Value,
    session_id: &str,
    shape: &CodexShape,
) -> Result<(Value, bool), ProviderError> {
    let requested_model = body.get("model").and_then(Value::as_str);
    let upstream_model = resolve_upstream_model(requested_model, &shape.model);
    if let Some(model) = requested_model {
        if model != upstream_model {
            tracing::debug!(
                client_model = model,
                "codex: model rewritten to {}",
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
            priority_tier: shape.fast,
            include_encrypted_reasoning: true,
            session_id,
        },
    )
}

/// Anthropic `system` (string or content-block array) → one instruction
/// string.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::sse::{SseTransform, StreamUsage};
    use serde_json::json;

    fn codex_credential() -> AccountCredential {
        AccountCredential::Codex {
            account_id: "acct-1".into(),
            access_token: "at-codex".into(),
            refresh_token: "rt-codex".into(),
            expires_at_ms: u64::MAX,
            last_refresh_ms: None,
        }
    }

    // ---- request translation ----

    #[test]
    fn translate_simple_text_request() {
        let body = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 1024,
            "stream": true,
            "system": "You are helpful.",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [{"type": "text", "text": "hello"}]},
                {"role": "user", "content": [{"type": "text", "text": "again"}]}
            ]
        });
        let (upstream, client_stream) = translate_request(&body, "sess-1").expect("translate");
        assert!(client_stream);
        assert_eq!(upstream["model"], CODEX_MODEL, "model always rewritten");
        assert_eq!(upstream["instructions"], "You are helpful.");
        assert_eq!(upstream["stream"], true);
        assert_eq!(upstream["store"], false);
        assert_eq!(upstream["parallel_tool_calls"], true);
        assert_eq!(upstream["prompt_cache_key"], "sess-1");
        assert_eq!(upstream["include"][0], "reasoning.encrypted_content");
        let input = upstream["input"].as_array().expect("input");
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][0]["text"], "hi");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"][0]["type"], "output_text");
        assert_eq!(input[1]["content"][0]["text"], "hello");
        assert_eq!(input[2]["content"][0]["text"], "again");
    }

    #[test]
    fn shape_sets_configurable_model_fast_tier_and_effort() {
        // An UNKNOWN client model keeps the configured pin (pass-through only
        // applies to known-valid upstream slugs).
        let body =
            json!({ "model": "my-custom-alias", "messages": [{"role":"user","content":"hi"}] });
        let shape = CodexShape {
            model: "gpt-5.5-codex".to_string(),
            client_model: None,
            fast: true,
            effort: Some("XHIGH".to_string()),
        };
        let (upstream, _) = translate_request_with(&body, "s", &shape).expect("translate");
        assert_eq!(upstream["model"], "gpt-5.5-codex", "model is config-driven");
        // codex stores "fast" but sends service_tier=priority on the wire.
        assert_eq!(upstream["service_tier"], "priority");
        // effort lowercased into reasoning.effort.
        assert_eq!(upstream["reasoning"]["effort"], "xhigh");
    }

    #[test]
    fn known_slugs_pass_through_and_bare_gpt56_maps_to_sol() {
        // Known-valid upstream slugs are forwarded verbatim — the client's
        // model choice is honored even when it differs from the pin.
        for slug in ["gpt-5.5", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
            let body = json!({ "model": slug, "messages": [{"role":"user","content":"hi"}] });
            let (upstream, _) =
                translate_request_with(&body, "s", &CodexShape::default()).expect("translate");
            assert_eq!(upstream["model"], slug, "known slug passes through");
        }
        // The bare gpt-5.6 id is rejected upstream — mapped to the sol tier.
        let body = json!({ "model": "GPT-5.6", "messages": [{"role":"user","content":"hi"}] });
        let (upstream, _) =
            translate_request_with(&body, "s", &CodexShape::default()).expect("translate");
        assert_eq!(upstream["model"], "gpt-5.6-sol");
        // Model-less requests keep the pin.
        let body = json!({ "messages": [{"role":"user","content":"hi"}] });
        let (upstream, _) =
            translate_request_with(&body, "s", &CodexShape::default()).expect("translate");
        assert_eq!(upstream["model"], CODEX_MODEL);
    }

    #[test]
    fn bare_variant_aliases_resolve_to_latest_generation() {
        // `sol`/`terra`/`luna` map to `gpt-5.6-<variant>` (latest generation),
        // case-insensitively, regardless of the configured pin.
        for (alias, expected) in [
            ("sol", "gpt-5.6-sol"),
            ("terra", "gpt-5.6-terra"),
            ("luna", "gpt-5.6-luna"),
            ("LUNA", "gpt-5.6-luna"),
        ] {
            let body = json!({ "model": alias, "messages": [{"role":"user","content":"hi"}] });
            let (upstream, _) =
                translate_request_with(&body, "s", &CodexShape::default()).expect("translate");
            assert_eq!(upstream["model"], expected, "alias {alias} → {expected}");
        }
    }

    #[test]
    fn request_meta_surfaces_per_request_effective_value() {
        // BYPASS shape (no configured effort): the client's value rides
        // through, including the clamp — `max` on gpt-5.5 (no extended
        // efforts) → `xhigh`.
        let bypass = CodexShape {
            model: "gpt-5.5".to_string(),
            client_model: None,
            fast: true,
            effort: None,
        };
        let body = json!({
            "model": "gpt-5.5",
            "output_config": { "effort": "max" },
            "messages": [{"role":"user","content":"hi"}],
        });
        let (model, effort, fast) = effective_request_meta(&body, &bypass);
        assert_eq!(model, "gpt-5.5");
        assert_eq!(effort.as_deref(), Some("xhigh"), "max clamps on gpt-5.5");
        assert!(fast, "fast mirrors the shape flag");

        // A CONFIGURED shape effort overrides the request (UI-3 U12) and the
        // alias resolves in the meta too.
        let pinned = CodexShape {
            effort: Some("low".to_string()),
            ..bypass.clone()
        };
        let body = json!({
            "model": "sol",
            "output_config": { "effort": "max" },
            "messages": [{"role":"user","content":"hi"}],
        });
        let (model, effort, _) = effective_request_meta(&body, &pinned);
        assert_eq!(model, "gpt-5.6-sol", "alias resolved in the recorded meta");
        assert_eq!(
            effort.as_deref(),
            Some("low"),
            "configured effort overrides"
        );
    }

    #[test]
    fn request_meta_records_the_resolved_model_not_the_shape_pin() {
        // Live regression: with the shape pinned to gpt-5.6-sol, a request for
        // gpt-5.5 was recorded as gpt-5.6-sol in the activity log even though
        // gpt-5.5 is what actually went upstream (and what the response
        // echoed). The recorded model must be the per-request resolved slug.
        let provider = CodexProvider::with_shape(
            "https://chatgpt.com/backend-api/codex",
            CodexShape {
                model: "gpt-5.6-sol".to_string(),
                client_model: None,
                fast: false,
                effort: None,
            },
        );
        let body = br#"{"model":"gpt-5.5","messages":[{"role":"user","content":"hi"}]}"#;
        let (model, _, _) = provider.request_meta(body);
        assert_eq!(model, "gpt-5.5", "resolved request model, not the pin");

        // A luna request that 404s upstream must still be RECORDED as luna —
        // that is exactly when the operator needs to see which model failed.
        let body = br#"{"model":"luna","messages":[{"role":"user","content":"hi"}]}"#;
        let (model, _, _) = provider.request_meta(body);
        assert_eq!(model, "gpt-5.6-luna");

        // Unknown / absent request models keep the pin.
        let body = br#"{"model":"my-alias","messages":[{"role":"user","content":"hi"}]}"#;
        let (model, _, _) = provider.request_meta(body);
        assert_eq!(model, "gpt-5.6-sol");
        let (model, _, _) = provider.request_meta(b"not json");
        assert_eq!(model, "gpt-5.6-sol", "non-JSON body falls back to the pin");
    }

    #[test]
    fn request_output_config_effort_wins_over_shape_and_maps_max() {
        // The Claude Agent SDK carries session effort as output_config.effort;
        // with NO configured override (bypass) it rides through.
        let shape = CodexShape {
            model: CODEX_MODEL.to_string(),
            client_model: None,
            fast: false,
            effort: None,
        };
        let body = json!({
            "model": "gpt-5.6-sol",
            "output_config": { "effort": "HIGH" },
            "messages": [{"role":"user","content":"hi"}],
        });
        let (upstream, _) = translate_request_with(&body, "s", &shape).expect("translate");
        assert_eq!(
            upstream["reasoning"]["effort"], "high",
            "bypass: request effort rides through"
        );

        // A configured effort OVERRIDES the request (UI-3 U12).
        let pinned = CodexShape {
            effort: Some("xhigh".to_string()),
            ..shape.clone()
        };
        let (upstream, _) = translate_request_with(&body, "s", &pinned).expect("translate");
        assert_eq!(
            upstream["reasoning"]["effort"], "xhigh",
            "configured effort overrides the request"
        );

        // The gpt-5.6 family natively supports max/ultra (codex catalog) —
        // pass through unclamped.
        for e in ["max", "ultra"] {
            let body = json!({
                "model": "gpt-5.6-sol",
                "output_config": { "effort": e },
                "messages": [{"role":"user","content":"hi"}],
            });
            let (upstream, _) = translate_request_with(&body, "s", &shape).expect("translate");
            assert_eq!(upstream["reasoning"]["effort"], e);
        }

        // Older models top out at xhigh — max clamps.
        let body = json!({
            "model": "gpt-5.5",
            "output_config": { "effort": "max" },
            "messages": [{"role":"user","content":"hi"}],
        });
        let (upstream, _) = translate_request_with(&body, "s", &shape).expect("translate");
        assert_eq!(upstream["reasoning"]["effort"], "xhigh");

        // Invalid request efforts under bypass yield no reasoning field at
        // all (nothing valid to ride through); with a configured override
        // the override applies regardless.
        let body = json!({
            "model": "gpt-5.6-sol",
            "output_config": { "effort": "turbo" },
            "messages": [{"role":"user","content":"hi"}],
        });
        let (upstream, _) = translate_request_with(&body, "s", &shape).expect("translate");
        assert_eq!(upstream["reasoning"]["effort"], serde_json::Value::Null);
        let (upstream, _) = translate_request_with(&body, "s", &pinned).expect("translate");
        assert_eq!(upstream["reasoning"]["effort"], "xhigh");
    }

    #[test]
    fn extended_efforts_boundary_is_exact_generation_not_a_bare_prefix() {
        // gpt-5.6 itself and its `-` variants get max/ultra.
        assert!(supports_extended_efforts("gpt-5.6"));
        assert!(supports_extended_efforts("gpt-5.6-sol"));
        assert!(supports_extended_efforts("GPT-5.6-TERRA"));
        // A hypothetical future gpt-5.60 must NOT match the 5.6 boundary —
        // its effort support is unknown, so `max` clamps like any other model.
        assert!(!supports_extended_efforts("gpt-5.60-sol"));
        assert!(!supports_extended_efforts("gpt-5.60"));
        assert!(!supports_extended_efforts("gpt-5.5"));
        let effort = resolve_reasoning_effort(
            &json!({ "output_config": { "effort": "max" } }),
            None,
            "gpt-5.60-sol",
        );
        assert_eq!(
            effort.as_deref(),
            Some("xhigh"),
            "max clamps on the unknown 5.60 generation"
        );
    }

    #[test]
    fn shape_default_omits_tier_and_reasoning() {
        // Unknown model id → pinned default; no tier/effort emitted.
        let body =
            json!({ "model": "unknown-model", "messages": [{"role":"user","content":"hi"}] });
        let (upstream, _) =
            translate_request_with(&body, "s", &CodexShape::default()).expect("translate");
        assert_eq!(upstream["model"], CODEX_MODEL);
        assert!(
            upstream.get("service_tier").is_none(),
            "no tier when not fast"
        );
        assert!(upstream.get("reasoning").is_none(), "no effort by default");
    }

    #[test]
    fn shape_treats_blank_or_default_effort_as_unset() {
        let body = json!({ "model": "x", "messages": [{"role":"user","content":"hi"}] });
        for e in ["", "  ", "default", "DEFAULT"] {
            let shape = CodexShape {
                model: CODEX_MODEL.to_string(),
                client_model: None,
                fast: false,
                effort: Some(e.to_string()),
            };
            let (upstream, _) = translate_request_with(&body, "s", &shape).expect("translate");
            assert!(
                upstream.get("reasoning").is_none(),
                "effort {e:?} should be treated as unset"
            );
        }
    }

    #[test]
    fn translate_system_blocks_join_to_instructions() {
        let body = json!({
            "system": [
                {"type": "text", "text": "Line one."},
                {"type": "text", "text": "Line two."}
            ],
            "messages": [{"role": "user", "content": "x"}]
        });
        let (upstream, client_stream) = translate_request(&body, "s").expect("translate");
        assert!(!client_stream, "stream defaults to false");
        assert_eq!(upstream["instructions"], "Line one.\nLine two.");
    }

    /// The codex `responses` endpoint rejects any `role:"system"` input item
    /// ("System messages are not allowed", verified live on :3477). A bare
    /// system-role message (the original P3 repro shape) must NOT become an
    /// input item — its text folds into `instructions` instead.
    #[test]
    fn translate_system_role_message_never_becomes_input_item() {
        let body = json!({
            "max_tokens": 40,
            "stream": true,
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "say OK"}
            ]
        });
        let (upstream, _) = translate_request(&body, "s").expect("translate");
        let input = upstream["input"].as_array().expect("input");
        // The system message produced no input item; only the user message did.
        assert_eq!(input.len(), 1, "system message must not emit an input item");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["text"], "say OK");
        // No input item may carry role:"system" (the codex 400 trigger).
        for item in input {
            assert_ne!(
                item["role"], "system",
                "no input item may have role:\"system\""
            );
        }
        // The system text is preserved as an instruction, not dropped.
        assert_eq!(upstream["instructions"], "be brief");
    }

    /// The exact shape Claude Code emits via the mid-conversation system beta
    /// (`mid-conversation-system-2026-04-07`): user → assistant → system → user.
    /// This was the live 400 repro; it must now translate cleanly with the
    /// mid-conversation system text folded after the top-level system prompt
    /// and zero `role:"system"` input items.
    #[test]
    fn translate_mid_conversation_system_message_folds_into_instructions() {
        let body = json!({
            "system": "You are helpful.",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [{"type": "text", "text": "hello"}]},
                {"role": "system", "content": "Terse mode enabled."},
                {"role": "user", "content": "say OK"}
            ]
        });
        let (upstream, _) = translate_request(&body, "s").expect("translate");
        let input = upstream["input"].as_array().expect("input");
        assert_eq!(input.len(), 3, "user, assistant, user — system folded out");
        let roles: Vec<&str> = input
            .iter()
            .filter(|i| i["type"] == "message")
            .map(|i| i["role"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(roles, vec!["user", "assistant", "user"]);
        assert!(
            input.iter().all(|i| i["role"] != "system"),
            "no role:\"system\" input item may survive"
        );
        // Top-level system first, mid-conversation system appended after it.
        assert_eq!(
            upstream["instructions"],
            "You are helpful.\nTerse mode enabled."
        );
    }

    /// Codex accepts `role:"developer"` (verified live: 200). A developer-role
    /// message passes through as a `developer` input item, not coerced to user
    /// and never to system.
    #[test]
    fn translate_developer_role_passes_through() {
        let body = json!({
            "messages": [
                {"role": "developer", "content": "be brief"},
                {"role": "user", "content": "say OK"}
            ]
        });
        let (upstream, _) = translate_request(&body, "s").expect("translate");
        let input = upstream["input"].as_array().expect("input");
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["role"], "developer");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][0]["text"], "be brief");
        assert_eq!(input[1]["role"], "user");
    }

    /// Any unrecognized role degrades to `user` — never to `system` (the one
    /// role codex forbids).
    #[test]
    fn translate_unknown_role_degrades_to_user_not_system() {
        let body = json!({
            "messages": [{"role": "tool", "content": "result text"}]
        });
        let (upstream, _) = translate_request(&body, "s").expect("translate");
        let input = upstream["input"].as_array().expect("input");
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], "user", "unknown role → user");
    }

    /// A system-role message expressed as a content-block array (Anthropic's
    /// other system shape) also folds into instructions, joining block text.
    #[test]
    fn translate_system_role_message_with_block_content_folds() {
        let body = json!({
            "messages": [
                {"role": "system", "content": [
                    {"type": "text", "text": "Rule one."},
                    {"type": "text", "text": "Rule two."}
                ]},
                {"role": "user", "content": "go"}
            ]
        });
        let (upstream, _) = translate_request(&body, "s").expect("translate");
        assert_eq!(upstream["instructions"], "Rule one.\nRule two.");
        let input = upstream["input"].as_array().expect("input");
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], "user");
    }

    #[test]
    fn translate_tool_use_and_tool_result_round() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "checking"},
                    {"type": "tool_use", "id": "call_1", "name": "get_weather",
                     "input": {"city": "Seoul"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_1",
                     "content": [{"type": "text", "text": "22C"}]}
                ]}
            ],
            "tools": [
                {"name": "get_weather", "description": "Get weather",
                 "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}}
            ],
            "tool_choice": {"type": "auto"}
        });
        let (upstream, _) = translate_request(&body, "s").expect("translate");
        let input = upstream["input"].as_array().expect("input");
        assert_eq!(input.len(), 4, "user text, assistant text, call, output");
        assert_eq!(input[1]["type"], "message");
        assert_eq!(input[1]["content"][0]["text"], "checking");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[2]["name"], "get_weather");
        let args: Value =
            serde_json::from_str(input[2]["arguments"].as_str().expect("args string"))
                .expect("args json");
        assert_eq!(args["city"], "Seoul");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "call_1");
        assert_eq!(input[3]["output"], "22C");

        let tools = upstream["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["name"], "get_weather");
        assert_eq!(tools[0]["description"], "Get weather");
        assert_eq!(tools[0]["strict"], false);
        assert_eq!(tools[0]["parameters"]["type"], "object");
        assert!(upstream.get("tool_choice").is_none(), "tool_choice ignored");
    }

    #[test]
    fn translate_drops_images_and_thinking() {
        let body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "image", "source": {"type": "base64", "data": "..."}},
                    {"type": "text", "text": "what is this"}
                ]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "hmm", "signature": "sig"},
                    {"type": "text", "text": "a cat"}
                ]}
            ]
        });
        let (upstream, _) = translate_request(&body, "s").expect("translate");
        let input = upstream["input"].as_array().expect("input");
        assert_eq!(input.len(), 2, "image and thinking dropped");
        assert_eq!(input[0]["content"][0]["text"], "what is this");
        assert_eq!(input[1]["content"][0]["text"], "a cat");
    }

    #[test]
    fn translate_rejects_missing_messages() {
        assert!(translate_request(&json!({"model": "m"}), "s").is_err());
    }

    #[test]
    fn build_request_sets_codex_headers() {
        let provider = CodexProvider::new("https://chatgpt.example/backend-api/codex");
        let body = json!({"stream": true, "messages": [{"role": "user", "content": "hi"}]});
        let (req, client_stream) = provider
            .build_request(body.to_string().as_bytes(), &codex_credential())
            .expect("build");
        assert!(client_stream);
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path, "/responses");
        assert_eq!(req.headers.get("authorization").unwrap(), "Bearer at-codex");
        assert_eq!(req.headers.get("chatgpt-account-id").unwrap(), "acct-1");
        assert_eq!(
            req.headers.get("openai-beta").unwrap(),
            "responses=experimental"
        );
        assert_eq!(req.headers.get("originator").unwrap(), "codex_cli_rs");
        assert_eq!(req.headers.get("accept").unwrap(), "text/event-stream");
        let session = req
            .headers
            .get("session-id")
            .and_then(|v| v.to_str().ok())
            .expect("session-id");
        assert_eq!(session.len(), 36, "uuid shape");
        let sent: Value = serde_json::from_slice(&req.body).expect("json");
        assert_eq!(sent["model"], CODEX_MODEL);
        assert_eq!(sent["prompt_cache_key"], session, "cache key = session id");
    }

    #[test]
    fn build_request_refuses_non_codex_credentials() {
        let provider = CodexProvider::new("https://x");
        let err = provider
            .build_request(
                br#"{"messages":[]}"#,
                &AccountCredential::Apikey {
                    api_key: "sk".into(),
                },
            )
            .unwrap_err();
        assert!(matches!(err, ProviderError::Auth(_)));
    }

    #[test]
    fn estimate_tokens_is_roughly_chars_over_four() {
        let body = json!({
            "system": "abcd",
            "messages": [{"role": "user", "content": "efghijkl"}]
        });
        // "user" (4) + content (8) + system (4) = 16 chars → 4 tokens.
        assert_eq!(estimate_input_tokens(&body), 4);
        assert_eq!(estimate_input_tokens(&json!({})), 1, "floor of 1");
    }

    // ---- SSE converter ----

    fn event(json: Value) -> String {
        format!(
            "event: {}\ndata: {json}",
            json["type"].as_str().unwrap_or("message")
        )
    }

    /// Feed a scripted sequence and split the emitted bytes back into
    /// `(event_type, data_json)` pairs for assertion.
    fn run_converter(events: &[Value]) -> (CodexSseConverter, Vec<(String, Value)>) {
        let mut converter = CodexSseConverter::new();
        let mut emitted = Vec::new();
        for e in events {
            emitted.extend_from_slice(&converter.on_event(&event(e.clone())));
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
            assert!(!event_type.is_empty(), "every event needs an event: line");
            let value: Value = serde_json::from_str(&data).expect("data is json");
            assert_eq!(
                value["type"], event_type,
                "data.type must match the event line"
            );
            parsed.push((event_type, value));
        }
        (converter, parsed)
    }

    fn types(events: &[(String, Value)]) -> Vec<&str> {
        events.iter().map(|(t, _)| t.as_str()).collect()
    }

    #[test]
    fn text_only_stream_maps_to_anthropic_sequence() {
        let (converter, events) = run_converter(&[
            json!({"type": "response.created", "response": {"id": "resp_1"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "delta": "Hel"}),
            json!({"type": "response.output_text.delta", "delta": "lo"}),
            json!({"type": "response.output_item.done", "item": {"type": "message"}}),
            json!({"type": "response.completed",
                   "response": {"usage": {"input_tokens": 12, "output_tokens": 5}}}),
        ]);
        assert_eq!(
            types(&events),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        let (_, start) = &events[0];
        assert_eq!(start["message"]["id"], "resp_1");
        assert_eq!(start["message"]["model"], CODEX_MODEL);
        let (_, block_start) = &events[1];
        assert_eq!(block_start["index"], 0);
        assert_eq!(block_start["content_block"]["type"], "text");
        assert_eq!(events[2].1["delta"]["text"], "Hel");
        assert_eq!(events[3].1["delta"]["text"], "lo");
        assert_eq!(events[4].1["index"], 0);
        let (_, message_delta) = &events[5];
        assert_eq!(message_delta["delta"]["stop_reason"], "end_turn");
        assert_eq!(message_delta["usage"]["input_tokens"], 12);
        assert_eq!(message_delta["usage"]["output_tokens"], 5);
        assert_eq!(
            converter.usage(),
            StreamUsage {
                input_tokens: 12,
                output_tokens: 5,
                // No cached_tokens key in the payload → unavailable, not zero.
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }
        );
    }

    #[test]
    fn cached_input_is_excluded_from_fresh_and_emitted_as_cache_read() {
        // OpenAI reports the cache-INCLUSIVE total in `input_tokens` with the
        // cached subset in `input_tokens_details.cached_tokens`. Record fresh =
        // total − cached (comparable to the Anthropic side, which counts
        // uncached input only) and surface the cached part as
        // `cache_read_input_tokens` so the client's context bar doesn't fill on
        // cache reads. Regression for the ~90× codex token inflation.
        let (converter, events) = run_converter(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "delta": "hi"}),
            json!({"type": "response.output_item.done", "item": {"type": "message"}}),
            json!({"type": "response.completed", "response": {"usage": {
                "input_tokens": 200_000,
                "input_tokens_details": {"cached_tokens": 199_000},
                "output_tokens": 42
            }}}),
        ]);
        let (_, message_delta) = events.iter().find(|(t, _)| t == "message_delta").unwrap();
        assert_eq!(
            message_delta["usage"]["input_tokens"], 1_000,
            "fresh = 200000 - 199000"
        );
        assert_eq!(message_delta["usage"]["cache_read_input_tokens"], 199_000);
        assert_eq!(message_delta["usage"]["output_tokens"], 42);
        // Dashboard totals read converter.usage(): fresh only, cached surfaced.
        assert_eq!(
            converter.usage(),
            StreamUsage {
                input_tokens: 1_000,
                output_tokens: 42,
                cache_read_input_tokens: Some(199_000),
                cache_creation_input_tokens: None,
            }
        );
    }

    #[test]
    fn live_wire_cached_split_matches_openai_responses_sample() {
        // Verbatim shape from the codex `/responses` upstream: `input_tokens`
        // is the cache-INCLUSIVE total, with the cached (and write) subsets in
        // `input_tokens_details`. fresh = 292455 − 100864 = 191591, and the
        // cached part surfaces as `cache_read_input_tokens` so activity/cost
        // stop billing the whole prompt at the full input rate.
        let (converter, events) = run_converter(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "delta": "hi"}),
            json!({"type": "response.output_item.done", "item": {"type": "message"}}),
            json!({"type": "response.completed", "response": {"usage": {
                "input_tokens": 292_455,
                "input_tokens_details": {"cache_write_tokens": 0, "cached_tokens": 100_864},
                "output_tokens": 77
            }}}),
        ]);
        let (_, message_delta) = events.iter().find(|(t, _)| t == "message_delta").unwrap();
        assert_eq!(
            message_delta["usage"]["input_tokens"], 191_591,
            "fresh = 292455 - 100864"
        );
        assert_eq!(message_delta["usage"]["cache_read_input_tokens"], 100_864);
        assert_eq!(message_delta["usage"]["cache_creation_input_tokens"], 0);
        assert_eq!(message_delta["usage"]["output_tokens"], 77);
        assert_eq!(
            converter.usage(),
            StreamUsage {
                input_tokens: 191_591,
                output_tokens: 77,
                cache_read_input_tokens: Some(100_864),
                // `cache_write_tokens` was present (0) → explicit Some(0).
                cache_creation_input_tokens: Some(0),
            }
        );
    }

    #[test]
    fn cached_greater_than_total_clamps_to_total() {
        // Defensive: a malformed payload where cached exceeds the reported
        // total must not underflow fresh input or surface a cache read larger
        // than the tokens received. fresh clamps to 0, cache_read to the total.
        let (converter, events) = run_converter(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "delta": "hi"}),
            json!({"type": "response.output_item.done", "item": {"type": "message"}}),
            json!({"type": "response.completed", "response": {"usage": {
                "input_tokens": 100,
                "input_tokens_details": {"cached_tokens": 150},
                "output_tokens": 3
            }}}),
        ]);
        let (_, message_delta) = events.iter().find(|(t, _)| t == "message_delta").unwrap();
        assert_eq!(message_delta["usage"]["input_tokens"], 0);
        assert_eq!(message_delta["usage"]["cache_read_input_tokens"], 100);
        assert_eq!(
            converter.usage(),
            StreamUsage {
                input_tokens: 0,
                output_tokens: 3,
                cache_read_input_tokens: Some(100),
                cache_creation_input_tokens: None,
            }
        );
    }

    #[test]
    fn explicit_zero_cached_keeps_full_input_and_some_zero_cache_read() {
        // cached_tokens=0 with no write field: fresh == total, cache_read is an
        // explicit Some(0) (reported, not unavailable), cache_creation absent.
        let (converter, _events) = run_converter(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "delta": "hi"}),
            json!({"type": "response.output_item.done", "item": {"type": "message"}}),
            json!({"type": "response.completed", "response": {"usage": {
                "input_tokens": 500,
                "input_tokens_details": {"cached_tokens": 0},
                "output_tokens": 9
            }}}),
        ]);
        assert_eq!(
            converter.usage(),
            StreamUsage {
                input_tokens: 500,
                output_tokens: 9,
                cache_read_input_tokens: Some(0),
                cache_creation_input_tokens: None,
            }
        );
    }

    #[test]
    fn tool_call_stream_emits_tool_use_block_and_stop_reason() {
        let (_, events) = run_converter(&[
            json!({"type": "response.created", "response": {"id": "resp_2"}}),
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
                            "name": "get_weather", "arguments": "{\"city\":\"Seoul\"}"}}),
            json!({"type": "response.completed",
                   "response": {"usage": {"input_tokens": 30, "output_tokens": 9}}}),
        ]);
        assert_eq!(
            types(&events),
            vec![
                "message_start",
                "content_block_start", // text
                "content_block_delta",
                "content_block_stop",
                "content_block_start", // tool_use
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        let (_, tool_start) = &events[4];
        assert_eq!(tool_start["index"], 1, "indexes sequence 0, 1");
        assert_eq!(tool_start["content_block"]["type"], "tool_use");
        assert_eq!(tool_start["content_block"]["id"], "call_9");
        assert_eq!(tool_start["content_block"]["name"], "get_weather");
        assert_eq!(events[5].1["delta"]["type"], "input_json_delta");
        assert_eq!(events[5].1["delta"]["partial_json"], "{\"city\":");
        assert_eq!(events[6].1["delta"]["partial_json"], "\"Seoul\"}");
        assert_eq!(events[7].1["index"], 1);
        assert_eq!(events[8].1["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn tool_arguments_only_at_item_done_still_emit_one_delta() {
        let (_, events) = run_converter(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "function_call", "call_id": "c1", "name": "f"}}),
            json!({"type": "response.output_item.done",
                   "item": {"type": "function_call", "call_id": "c1", "name": "f",
                            "arguments": "{\"a\":1}"}}),
            json!({"type": "response.completed", "response": {"usage": {"input_tokens": 1, "output_tokens": 1}}}),
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
            ]
        );
        assert_eq!(events[2].1["delta"]["partial_json"], "{\"a\":1}");
    }

    #[test]
    fn reasoning_deltas_open_and_close_a_thinking_block() {
        let (_, events) = run_converter(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.reasoning_summary_text.delta", "delta": "let me think"}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "delta": "answer"}),
            json!({"type": "response.completed", "response": {"usage": {"input_tokens": 2, "output_tokens": 2}}}),
        ]);
        assert_eq!(
            types(&events),
            vec![
                "message_start",
                "content_block_start", // thinking (index 0)
                "content_block_delta",
                "content_block_stop",  // thinking closed when text starts
                "content_block_start", // text (index 1)
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[1].1["content_block"]["type"], "thinking");
        assert_eq!(events[1].1["index"], 0);
        assert_eq!(events[2].1["delta"]["type"], "thinking_delta");
        assert_eq!(events[2].1["delta"]["thinking"], "let me think");
        assert_eq!(events[4].1["content_block"]["type"], "text");
        assert_eq!(events[4].1["index"], 1);
    }

    #[test]
    fn upstream_error_event_maps_to_anthropic_error_and_terminates() {
        let (converter, events) = run_converter(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "error", "message": "quota exceeded"}),
            // Anything after the error must be swallowed.
            json!({"type": "response.output_text.delta", "delta": "x"}),
        ]);
        assert_eq!(types(&events), vec!["message_start", "error"]);
        assert_eq!(events[1].1["error"]["type"], "api_error");
        assert_eq!(events[1].1["error"]["message"], "quota exceeded");
        assert_eq!(converter.error_message(), Some("quota exceeded"));
    }

    #[test]
    fn response_failed_maps_to_error() {
        let (_, events) = run_converter(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.failed",
                   "response": {"error": {"message": "server melted"}}}),
        ]);
        assert_eq!(types(&events), vec!["message_start", "error"]);
        assert_eq!(events[1].1["error"]["message"], "server melted");
    }

    #[test]
    fn truncated_stream_emits_error_on_end() {
        let (_, events) = run_converter(&[
            json!({"type": "response.created", "response": {"id": "r"}}),
            json!({"type": "response.output_text.delta", "delta": "hal"}),
        ]);
        let kinds = types(&events);
        assert_eq!(
            kinds.last(),
            Some(&"error"),
            "missing response.completed must not look like a clean end: {kinds:?}"
        );
    }

    #[test]
    fn aggregate_builds_non_streaming_message_json() {
        let (converter, _) = run_converter(&[
            json!({"type": "response.created", "response": {"id": "resp_agg"}}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "delta": "The answer "}),
            json!({"type": "response.output_text.delta", "delta": "is 42."}),
            json!({"type": "response.output_item.done", "item": {"type": "message"}}),
            json!({"type": "response.output_item.added",
                   "item": {"type": "function_call", "call_id": "c2", "name": "save"}}),
            json!({"type": "response.function_call_arguments.delta", "delta": "{\"v\":42}"}),
            json!({"type": "response.output_item.done", "item": {"type": "function_call"}}),
            json!({"type": "response.completed",
                   "response": {"usage": {"input_tokens": 7, "output_tokens": 3}}}),
        ]);
        let message = converter.into_message_json().expect("message");
        assert_eq!(message["id"], "resp_agg");
        assert_eq!(message["model"], CODEX_MODEL);
        assert_eq!(message["stop_reason"], "tool_use");
        assert_eq!(message["content"][0]["type"], "text");
        assert_eq!(message["content"][0]["text"], "The answer is 42.");
        assert_eq!(message["content"][1]["type"], "tool_use");
        assert_eq!(message["content"][1]["id"], "c2");
        assert_eq!(message["content"][1]["name"], "save");
        assert_eq!(message["content"][1]["input"]["v"], 42);
        assert_eq!(message["usage"]["input_tokens"], 7);
        assert_eq!(message["usage"]["output_tokens"], 3);
    }

    #[test]
    fn aggregate_of_failed_stream_is_none() {
        let (converter, _) = run_converter(&[json!({"type": "error", "message": "nope"})]);
        assert!(converter.into_message_json().is_none());
    }

    /// Like [`run_converter`] but drives a caller-supplied converter, so a
    /// `client_model` override can be exercised through the same stream path.
    fn drive_converter(
        mut converter: CodexSseConverter,
        events: &[Value],
    ) -> (CodexSseConverter, Vec<(String, Value)>) {
        let mut emitted = Vec::new();
        for e in events {
            emitted.extend_from_slice(&converter.on_event(&event(e.clone())));
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
                }
            }
            let value: Value = serde_json::from_str(&data).expect("data is json");
            parsed.push((event_type, value));
        }
        (converter, parsed)
    }

    /// A minimal text-only stream ending in `response.completed`, enough to
    /// emit a `message_start` and build the non-stream aggregate.
    fn minimal_completed_stream() -> Vec<Value> {
        vec![
            json!({"type": "response.created", "response": {"id": "resp_cm"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.output_text.delta", "delta": "hi"}),
            json!({"type": "response.output_item.done", "item": {"type": "message"}}),
            json!({"type": "response.completed",
                   "response": {"usage": {"input_tokens": 9, "output_tokens": 2}}}),
        ]
    }

    #[test]
    fn client_model_override_restamps_the_response_model() {
        // client_model = Some(...) must rewrite BOTH client-facing stamps: the
        // streamed message_start and the non-stream aggregate.
        let converter = CodexSseConverter::with_model("gpt-5.5".to_string())
            .with_client_model(Some("claude-opus-4-8".to_string()));
        let (converter, events) = drive_converter(converter, &minimal_completed_stream());

        let (_, start) = events
            .iter()
            .find(|(t, _)| t == "message_start")
            .expect("message_start emitted");
        assert_eq!(
            start["message"]["model"], "claude-opus-4-8",
            "streamed message_start must carry the client_model override"
        );

        let message = converter.into_message_json().expect("message");
        assert_eq!(
            message["model"], "claude-opus-4-8",
            "non-stream aggregate must carry the client_model override"
        );
    }

    #[test]
    fn client_model_none_keeps_real_model() {
        // Default (no override) → both stamps report the real codex model.
        let converter = CodexSseConverter::with_model("gpt-5.5".to_string());
        let (converter, events) = drive_converter(converter, &minimal_completed_stream());

        let (_, start) = events
            .iter()
            .find(|(t, _)| t == "message_start")
            .expect("message_start emitted");
        assert_eq!(
            start["message"]["model"], "gpt-5.5",
            "with no override, message_start reports the real codex model"
        );

        let message = converter.into_message_json().expect("message");
        assert_eq!(message["model"], "gpt-5.5");
    }

    #[test]
    fn client_model_override_does_not_change_the_provider_real_model() {
        // The override is a RESPONSE-STAMP concern only. Routing, the dashboard
        // (RequestRouted), the trace log, and the scheduler all read
        // `CodexProvider::model()`, which must stay the real upstream model so
        // a codex session is still attributed to codex — never to the spoofed
        // claude name. This guards that the client_model knob can't leak into
        // the provider's reported model.
        let shape = CodexShape {
            model: "gpt-5.5".to_string(),
            client_model: Some("claude-opus-4-8".to_string()),
            fast: false,
            effort: None,
        };
        let provider = CodexProvider::with_shape("http://unused", shape);
        assert_eq!(
            provider.model(),
            "gpt-5.5",
            "provider.model() (routing/dashboard/trace source) must be the REAL model"
        );
        // The converter it hands out, however, stamps the override client-side.
        let (_, events) = drive_converter(provider.converter(), &minimal_completed_stream());
        let (_, start) = events
            .iter()
            .find(|(t, _)| t == "message_start")
            .expect("message_start emitted");
        assert_eq!(start["message"]["model"], "claude-opus-4-8");
    }

    #[test]
    fn live_captured_sequence_maps_to_clean_anthropic_stream() {
        // Event shapes from the 2026-06-12 live chatgpt.com smoke capture:
        // a reasoning item with encrypted_content and an EMPTY summary (no
        // reasoning_summary_text.delta ever fires), a message item tagged
        // phase:"final_answer", obfuscation fields on every text delta, and
        // the in_progress / content_part.* / output_text.done bookkeeping
        // events Phase C's scripted tests never exercised. None of the
        // ignorable events may produce malformed or stray blocks.
        let (converter, events) = run_converter(&[
            json!({"type": "response.created",
                   "response": {"id": "resp_live", "object": "response",
                                "status": "in_progress", "model": "gpt-5.5",
                                "output": [], "usage": null}}),
            json!({"type": "response.in_progress",
                   "response": {"id": "resp_live", "status": "in_progress"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"id": "rs_live_1", "type": "reasoning",
                            "encrypted_content": "gAAAAA-opaque", "summary": []}}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"id": "rs_live_1", "type": "reasoning",
                            "encrypted_content": "gAAAAA-opaque", "summary": []}}),
            json!({"type": "response.output_item.added", "output_index": 1,
                   "item": {"id": "msg_live_1", "type": "message",
                            "status": "in_progress", "content": [],
                            "phase": "final_answer", "role": "assistant"}}),
            json!({"type": "response.content_part.added", "content_index": 0,
                   "item_id": "msg_live_1", "output_index": 1,
                   "part": {"type": "output_text", "annotations": [],
                            "logprobs": [], "text": ""}}),
            json!({"type": "response.output_text.delta", "content_index": 0,
                   "delta": "O", "item_id": "msg_live_1", "logprobs": [],
                   "obfuscation": "ydFpcUg7ZI1oyX", "output_index": 1}),
            json!({"type": "response.output_text.delta", "content_index": 0,
                   "delta": "K", "item_id": "msg_live_1", "logprobs": [],
                   "obfuscation": "x91js", "output_index": 1}),
            json!({"type": "response.output_text.delta", "content_index": 0,
                   "delta": ", ", "item_id": "msg_live_1", "logprobs": [],
                   "obfuscation": "p2", "output_index": 1}),
            json!({"type": "response.output_text.delta", "content_index": 0,
                   "delta": "done", "item_id": "msg_live_1", "logprobs": [],
                   "obfuscation": "qq8", "output_index": 1}),
            json!({"type": "response.output_text.done", "content_index": 0,
                   "item_id": "msg_live_1", "logprobs": [], "output_index": 1,
                   "text": "OK, done"}),
            json!({"type": "response.content_part.done", "content_index": 0,
                   "item_id": "msg_live_1", "output_index": 1,
                   "part": {"type": "output_text", "annotations": [],
                            "logprobs": [], "text": "OK, done"}}),
            json!({"type": "response.output_item.done", "output_index": 1,
                   "item": {"id": "msg_live_1", "type": "message",
                            "status": "completed",
                            "content": [{"type": "output_text", "text": "OK, done"}],
                            "phase": "final_answer", "role": "assistant"}}),
            json!({"type": "response.completed",
                   "response": {"id": "resp_live", "status": "completed",
                                "usage": {"input_tokens": 8,
                                          "input_tokens_details": {"cached_tokens": 0},
                                          "output_tokens": 5,
                                          "total_tokens": 13}}}),
        ]);
        assert_eq!(
            types(&events),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[0].1["message"]["id"], "resp_live");
        assert_eq!(events[1].1["content_block"]["type"], "text");
        assert_eq!(
            events[1].1["index"], 0,
            "reasoning item must not burn an index"
        );
        for (i, expected) in [(2, "O"), (3, "K"), (4, ", "), (5, "done")] {
            assert_eq!(events[i].1["delta"]["type"], "text_delta");
            assert_eq!(events[i].1["delta"]["text"], expected);
        }
        assert_eq!(events[7].1["delta"]["stop_reason"], "end_turn");
        assert_eq!(events[7].1["usage"]["input_tokens"], 8);
        assert_eq!(events[7].1["usage"]["output_tokens"], 5);
        assert!(converter.error_message().is_none());
        assert_eq!(
            converter.usage(),
            StreamUsage {
                input_tokens: 8,
                output_tokens: 5,
                // The live payload reports input_tokens_details.cached_tokens=0,
                // so cache-read is an explicit Some(0), not unavailable.
                cache_read_input_tokens: Some(0),
                cache_creation_input_tokens: None,
            }
        );
    }

    #[test]
    fn json_body_instead_of_sse_terminates_with_clean_error_event() {
        // relay_codex trusts every 2xx to be SSE; a plain JSON body yields
        // zero parseable events (the EventBuffer hands the whole document
        // over as one terminal remainder with no `data:` lines). The
        // converter must end the client stream with a clean Anthropic error
        // event — never silence, never garbage.
        let mut converter = CodexSseConverter::new();
        assert!(
            converter
                .on_event(r#"{"detail":"not an event stream"}"#)
                .is_empty(),
            "a non-SSE body produces no downstream events"
        );
        let out = converter.on_end();
        let text = String::from_utf8(out).expect("utf8");
        let chunks: Vec<&str> = text
            .split("\n\n")
            .filter(|c| !c.trim().is_empty())
            .collect();
        assert_eq!(chunks.len(), 1, "exactly one terminal event: {text:?}");
        assert!(chunks[0].starts_with("event: error\n"), "{text:?}");
        let data: Value = serde_json::from_str(
            chunks[0]
                .lines()
                .find_map(|l| l.strip_prefix("data: "))
                .expect("data line"),
        )
        .expect("valid json");
        assert_eq!(data["type"], "error");
        assert_eq!(data["error"]["type"], "api_error");
        assert_eq!(
            converter.error_message(),
            Some("codex upstream returned no SSE events")
        );
        // The non-streaming aggregate path must refuse to fabricate an
        // empty 200 message out of it.
        assert!(converter.into_message_json().is_none());
    }

    #[test]
    fn done_marker_and_garbage_data_are_ignored() {
        let mut converter = CodexSseConverter::new();
        assert!(converter.on_event("data: [DONE]").is_empty());
        assert!(converter.on_event("data: {not json").is_empty());
        assert!(converter.on_event(": keepalive comment").is_empty());
    }
}
