//! `AnthropicPassthrough` — the v0.1 working provider. Conversion hooks are
//! byte-identity in the common case (the unified types wrap the Anthropic wire
//! shape, and `bytes::Bytes` clones are refcounted); only Claude-Code-local
//! model annotations are normalized before the request leaves the proxy.
//!
//! Where the hourglass would engage for a NON-passthrough provider:
//! `request_out` would parse the Anthropic body into real unified fields
//! (messages, tools, system), `request_in` would serialize them into the
//! provider's native shape, and `response_in`/`response_out` would convert
//! back — `forward.rs` already routes every request through these four hooks
//! plus `endpoint()`/`auth()`, so a future provider slots in without touching
//! the proxy core.

use http::header::AUTHORIZATION;
use http::{HeaderMap, HeaderValue};

use super::{
    AnthropicRequest, AnthropicResponse, Provider, ProviderError, ProviderRequest,
    ProviderResponse, UnifiedRequest, UnifiedResponse,
};
use crate::config::AccountCredential;

/// Normalize an Anthropic-bound JSON body in ONE parse: resolve a curated
/// claude ALIAS in `model` to its catalog id, strip the Claude-Code-local
/// `[1m]` context-window suffix from `model`, and strip foreign (unsigned)
/// thinking blocks from `messages` (issue #116). The two model steps run in
/// that order and compose (`opus` → `claude-opus-5[1m]` → `claude-opus-5`).
/// Because the alias step runs FIRST, an alias that already carries the client
/// suffix (`opus[1m]`, `fable[1m]`) must resolve too — that is handled on the
/// needle side in [`crate::catalog::resolve_claude_alias`].
/// Returns the original bytes (refcounted, byte-identity) when nothing
/// changed; a non-JSON body passes through untouched — passthrough never
/// fails on body shape.
fn normalize_body(body: bytes::Bytes) -> bytes::Bytes {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let alias = resolve_client_alias(&mut value);
    let model = strip_client_context_suffix(&mut value);
    let thinking = strip_foreign_thinking(&mut value);
    if !(alias || model || thinking) {
        return body;
    }
    match serde_json::to_vec(&value) {
        Ok(bytes) => bytes::Bytes::from(bytes),
        Err(_) => body,
    }
}

/// Rewrite a client-supplied model that is a curated claude ALIAS (`opus`,
/// `sonnet`, `haiku`, …) to its catalog id, so the alias `GET /models`
/// advertises is actually honored upstream: before this hook a bare `opus` was
/// forwarded verbatim and 404'd at api.anthropic.com. Mapping lives in
/// [`crate::catalog::CLAUDE_MODELS`] behind
/// [`crate::catalog::resolve_claude_alias`], so a new curated row carries its
/// aliases automatically and anything that is not an alias (a real id, a
/// foreign slug) passes through untouched — passthrough stays passthrough.
fn resolve_client_alias(value: &mut serde_json::Value) -> bool {
    let Some(model) = value.get("model").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Some(id) = crate::catalog::resolve_claude_alias(model) else {
        return false;
    };
    value["model"] = serde_json::Value::String(id.to_string());
    true
}

fn strip_client_context_suffix(value: &mut serde_json::Value) -> bool {
    let Some(model) = value.get("model").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Some(base) = model.strip_suffix("[1m]") else {
        return false;
    };
    let base = base.to_string();
    value["model"] = serde_json::Value::String(base);
    true
}

/// Strip `thinking` blocks the real Anthropic API cannot verify — blocks with
/// a MISSING or EMPTY `signature` — from `messages` (issue #116). The
/// responses-family translator (grok/codex) synthesizes thinking blocks
/// without a signature (it has nothing to sign them with), the client stores
/// them in session history, and after a mid-session model switch the replay
/// reaches api.anthropic.com, which rejects the request with 400
/// `Invalid signature in thinking block`. Anthropic-signed blocks carry a
/// non-empty signature and pass through untouched; `redacted_thinking`
/// (signed inside its `data`, no `signature` field by design) is never
/// touched. A message whose content array is left EMPTY by the strip is
/// dropped whole — a foreign thinking-only turn has nothing valid to replay,
/// and an empty content array is itself an upstream 400.
pub(crate) fn strip_foreign_thinking(value: &mut serde_json::Value) -> bool {
    let Some(messages) = value
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    let mut changed = false;
    messages.retain_mut(|message| {
        let Some(content) = message
            .get_mut("content")
            .and_then(serde_json::Value::as_array_mut)
        else {
            return true;
        };
        let before = content.len();
        content.retain(|block| {
            block.get("type").and_then(serde_json::Value::as_str) != Some("thinking")
                || block
                    .get("signature")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|signature| !signature.is_empty())
        });
        if content.len() == before {
            return true;
        }
        changed = true;
        !content.is_empty()
    });
    if changed {
        tracing::info!(
            "stripped unsigned (translate-synthesized) thinking blocks from an \
             anthropic-bound request (issue #116)"
        );
    }
    changed
}

/// Client auth header stripped/replaced on the way upstream.
pub const X_API_KEY: &str = "x-api-key";

/// Strip client-supplied auth (`x-api-key` / `authorization`) and inject the
/// selected account's credential: `Authorization: Bearer <token>` for oauth,
/// `x-api-key: <key>` for apikey (FR1). A credential that cannot be encoded
/// as a header value is an auth error (never send the client's own auth
/// through by accident).
pub fn inject_credential(
    headers: &mut HeaderMap,
    credential: &AccountCredential,
) -> Result<(), ProviderError> {
    headers.remove(X_API_KEY);
    headers.remove(AUTHORIZATION);
    match credential {
        AccountCredential::Oauth { access_token, .. } => {
            let value = HeaderValue::from_str(&format!("Bearer {access_token}"))
                .map_err(|err| ProviderError::Auth(err.to_string()))?;
            headers.insert(AUTHORIZATION, value);
        }
        AccountCredential::Apikey { api_key } => {
            let value = HeaderValue::from_str(api_key)
                .map_err(|err| ProviderError::Auth(err.to_string()))?;
            headers.insert(X_API_KEY, value);
        }
        // A codex credential must never leak to the Anthropic upstream —
        // the proxy routes codex accounts through the codex provider before
        // this point; reaching here is a routing bug.
        AccountCredential::Grok { .. } => {
            return Err(ProviderError::Auth(
                "grok credential cannot authenticate against the anthropic provider".into(),
            ));
        }
        AccountCredential::Codex { .. } => {
            return Err(ProviderError::Auth(
                "codex credential cannot authenticate against the anthropic provider".into(),
            ));
        }
        AccountCredential::OpenRouter { .. } => {
            return Err(ProviderError::Auth(
                "openrouter credential cannot authenticate against the anthropic provider".into(),
            ));
        }
    }
    Ok(())
}

/// Identity transformer for the real Anthropic API.
#[derive(Debug, Clone)]
pub struct AnthropicPassthrough {
    /// Upstream base URL (config `upstream`, default `https://api.anthropic.com`).
    base_url: String,
}

impl AnthropicPassthrough {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl Provider for AnthropicPassthrough {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn endpoint(&self) -> &str {
        &self.base_url
    }

    /// Strip client-supplied `x-api-key` / `authorization`, inject the
    /// selected account's credential (Bearer for oauth, x-api-key for
    /// apikey).
    async fn auth(
        &self,
        req: &mut ProviderRequest,
        account: &AccountCredential,
    ) -> Result<(), ProviderError> {
        inject_credential(&mut req.headers, account)
    }

    /// Identity wrap. Extracts `model` and `stream` from the JSON body when
    /// present without touching the body bytes; a non-JSON body simply yields
    /// no flags — passthrough never fails on body shape. `model` is now the
    /// live backend-group routing key (see `routing.rs`); the extraction
    /// itself is shared via `routing::model_from_body`.
    fn request_out(
        &self,
        anthropic_req: AnthropicRequest,
    ) -> Result<UnifiedRequest, ProviderError> {
        // `model` extraction is shared with the proxy's routing path (one
        // source of truth: `routing::model_from_body`); `stream` stays local.
        let model = crate::routing::model_from_body(&anthropic_req.body);
        let stream = serde_json::from_slice::<serde_json::Value>(&anthropic_req.body)
            .ok()
            .and_then(|value| value.get("stream").and_then(serde_json::Value::as_bool))
            .unwrap_or(false);
        Ok(UnifiedRequest {
            model,
            stream,
            wire: anthropic_req,
        })
    }

    /// Normalize the Claude-Code-only context-window suffix and strip foreign
    /// (unsigned) thinking blocks (issue #116) in one parse; otherwise unwrap
    /// without reserializing (moves the original wire body out).
    fn request_in(&self, unified: UnifiedRequest) -> Result<ProviderRequest, ProviderError> {
        let wire = unified.wire;
        Ok(ProviderRequest {
            method: wire.method,
            path: wire.path,
            headers: wire.headers,
            body: normalize_body(wire.body),
        })
    }

    /// Identity wrap.
    fn response_in(
        &self,
        provider_resp: ProviderResponse,
    ) -> Result<UnifiedResponse, ProviderError> {
        Ok(UnifiedResponse {
            wire: AnthropicResponse {
                status: provider_resp.status,
                headers: provider_resp.headers,
                body: provider_resp.body,
            },
        })
    }

    /// Identity unwrap.
    fn response_out(&self, unified: UnifiedResponse) -> Result<AnthropicResponse, ProviderError> {
        Ok(unified.wire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Method;

    fn provider() -> AnthropicPassthrough {
        AnthropicPassthrough::new("https://api.anthropic.com")
    }

    fn request(body: &str) -> AnthropicRequest {
        AnthropicRequest {
            method: Method::POST,
            path: "/v1/messages".to_string(),
            headers: HeaderMap::new(),
            body: bytes::Bytes::copy_from_slice(body.as_bytes()),
        }
    }

    #[test]
    fn request_out_extracts_model_and_stream() {
        let unified = provider()
            .request_out(request(
                r#"{"model":"claude-sonnet-4-5","stream":true,"messages":[]}"#,
            ))
            .expect("unified");
        assert_eq!(unified.model.as_deref(), Some("claude-sonnet-4-5"));
        assert!(unified.stream);
    }

    #[test]
    fn request_out_tolerates_non_json_bodies() {
        let unified = provider()
            .request_out(request("not json"))
            .expect("unified");
        assert_eq!(unified.model, None);
        assert!(!unified.stream);
    }

    #[test]
    fn round_trip_is_byte_identical() {
        let body = r#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#;
        let p = provider();
        let unified = p.request_out(request(body)).expect("out");
        let provider_req = p.request_in(unified).expect("in");
        assert_eq!(provider_req.body.as_ref(), body.as_bytes());
        assert_eq!(provider_req.path, "/v1/messages");
        assert_eq!(provider_req.method, Method::POST);
    }

    #[test]
    fn request_in_strips_client_context_suffix_from_claude_model() {
        let body = r#"{"model":"claude-opus-4-8[1m]","messages":[{"role":"user","content":"hi"}]}"#;
        let p = provider();
        let unified = p.request_out(request(body)).expect("out");
        let provider_req = p.request_in(unified).expect("in");
        let upstream: serde_json::Value =
            serde_json::from_slice(&provider_req.body).expect("upstream json");
        assert_eq!(upstream["model"], "claude-opus-4-8");
    }

    /// Run a `model` value through `normalize_body` and return what the
    /// upstream would actually receive.
    fn normalized_model(model: &str) -> String {
        let body = serde_json::json!({"model": model, "messages": []}).to_string();
        let out = normalize_body(bytes::Bytes::from(body));
        let value: serde_json::Value = serde_json::from_slice(&out).expect("upstream json");
        value["model"].as_str().expect("model string").to_string()
    }

    /// The curated aliases `GET /models` advertises must be honored upstream:
    /// a bare `opus` used to be forwarded verbatim and 404 at
    /// api.anthropic.com. Alias resolution runs BEFORE the `[1m]` strip, so
    /// the two compose into the real upstream id.
    #[test]
    fn normalize_body_resolves_curated_claude_aliases() {
        assert_eq!(normalized_model("opus"), "claude-opus-5");
        assert_eq!(normalized_model("opus-5"), "claude-opus-5");
        assert_eq!(normalized_model("sonnet"), "claude-sonnet-5");
        assert_eq!(normalized_model("sonnet-5"), "claude-sonnet-5");
        assert_eq!(normalized_model("fable"), "claude-fable-5-1");
        assert_eq!(normalized_model("fable-5-1"), "claude-fable-5-1");
    }

    /// An alias may arrive with the Claude-Code-local `[1m]` suffix attached.
    /// Alias resolution runs BEFORE the suffix strip, so before the fix
    /// `opus[1m]` matched no alias, was then reduced to a bare `opus`, and
    /// 404'd at api.anthropic.com.
    #[test]
    fn normalize_body_resolves_an_alias_carrying_the_client_context_suffix() {
        assert_eq!(normalized_model("fable[1m]"), "claude-fable-5-1");
        assert_eq!(normalized_model("opus[1m]"), "claude-opus-5");
    }

    /// `haiku`'s catalog row carries NO `[1m]` suffix — proof the two steps
    /// compose rather than both always firing.
    #[test]
    fn normalize_body_resolves_an_alias_whose_id_has_no_context_suffix() {
        assert_eq!(normalized_model("haiku"), "claude-haiku-4-5");
    }

    #[test]
    fn normalize_body_resolves_aliases_case_insensitively_and_trimmed() {
        assert_eq!(normalized_model("  OPUS  "), "claude-opus-5");
    }

    /// A real id is not an alias: only the pre-existing suffix strip applies.
    #[test]
    fn normalize_body_leaves_a_real_id_to_the_suffix_strip() {
        assert_eq!(normalized_model("claude-opus-5[1m]"), "claude-opus-5");
    }

    /// A real id with no suffix and no alias match changes nothing — the body
    /// must come back byte-identical (refcounted fast path).
    #[test]
    fn normalize_body_is_byte_identical_for_a_plain_id() {
        let body = r#"{"model":"claude-opus-4-8","messages":[]}"#;
        let out = normalize_body(bytes::Bytes::from_static(body.as_bytes()));
        assert_eq!(out.as_ref(), body.as_bytes());
    }

    /// A non-claude slug is never rewritten (grok/codex own their own
    /// resolution).
    #[test]
    fn normalize_body_never_rewrites_a_foreign_slug() {
        let body = r#"{"model":"grok-4.5","messages":[]}"#;
        let out = normalize_body(bytes::Bytes::from_static(body.as_bytes()));
        assert_eq!(out.as_ref(), body.as_bytes());
    }

    #[test]
    fn normalize_body_passes_non_json_through_byte_identical() {
        let out = normalize_body(bytes::Bytes::from_static(b"not json"));
        assert_eq!(out.as_ref(), b"not json");
    }

    /// Issue #116 RED shape: the exact replay a client sends after a grok
    /// turn — the responses-family translator synthesized a `thinking` block
    /// with NO signature, the client stored it, and the next (claude-routed)
    /// request replays it. Passing it through verbatim is the production 400
    /// (`Invalid signature in thinking block` from api.anthropic.com).
    #[test]
    fn request_in_strips_unsigned_foreign_thinking_blocks() {
        let body = r#"{"model":"claude-sonnet-5","thinking":{"type":"disabled"},"messages":[{"role":"user","content":"hi"},{"role":"assistant","content":[{"type":"thinking","thinking":"grok reasoning"},{"type":"text","text":"grok answer"}]},{"role":"user","content":"continue"}]}"#;
        let p = provider();
        let unified = p.request_out(request(body)).expect("out");
        let provider_req = p.request_in(unified).expect("in");
        let upstream: serde_json::Value =
            serde_json::from_slice(&provider_req.body).expect("upstream json");
        let assistant = &upstream["messages"][1]["content"];
        assert_eq!(
            assistant.as_array().map(Vec::len),
            Some(1),
            "unsigned thinking stripped: {assistant}"
        );
        assert_eq!(assistant[0]["type"], "text", "the real content survives");
        assert_eq!(
            upstream["messages"].as_array().map(Vec::len),
            Some(3),
            "no message dropped when content remains"
        );
        // The `thinking` REQUEST PARAM is configuration, not a content block —
        // untouched.
        assert_eq!(upstream["thinking"]["type"], "disabled");
    }

    /// An empty-string signature is the other foreign shape (a client that
    /// serializes the missing field as `""`) — same strip.
    #[test]
    fn request_in_strips_empty_signature_thinking_blocks() {
        let body = r#"{"model":"claude-sonnet-5","messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"x","signature":""},{"type":"text","text":"t"}]}]}"#;
        let p = provider();
        let unified = p.request_out(request(body)).expect("out");
        let provider_req = p.request_in(unified).expect("in");
        let upstream: serde_json::Value =
            serde_json::from_slice(&provider_req.body).expect("upstream json");
        assert_eq!(
            upstream["messages"][0]["content"].as_array().map(Vec::len),
            Some(1)
        );
    }

    /// Anthropic's OWN thinking blocks (non-empty signature) and
    /// `redacted_thinking` (no signature field by design) must ride through
    /// byte-identical — the strip is for foreign blocks only.
    #[test]
    fn request_in_keeps_signed_and_redacted_thinking_byte_identical() {
        let body = r#"{"model":"claude-sonnet-5","messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"real","signature":"EqQBCkYIChgCIkDRV7..."},{"type":"redacted_thinking","data":"opaque"},{"type":"text","text":"t"}]}]}"#;
        let p = provider();
        let unified = p.request_out(request(body)).expect("out");
        let provider_req = p.request_in(unified).expect("in");
        assert_eq!(
            provider_req.body.as_ref(),
            body.as_bytes(),
            "nothing foreign → byte-identity fast path"
        );
    }

    /// A foreign thinking-ONLY assistant turn has nothing valid left after
    /// the strip; an empty content array is itself an upstream 400, so the
    /// whole message is dropped.
    #[test]
    fn request_in_drops_a_message_left_empty_by_the_strip() {
        let body = r#"{"model":"claude-sonnet-5","messages":[{"role":"user","content":"hi"},{"role":"assistant","content":[{"type":"thinking","thinking":"only"}]},{"role":"user","content":"next"}]}"#;
        let p = provider();
        let unified = p.request_out(request(body)).expect("out");
        let provider_req = p.request_in(unified).expect("in");
        let upstream: serde_json::Value =
            serde_json::from_slice(&provider_req.body).expect("upstream json");
        let messages = upstream["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2, "thinking-only turn dropped: {upstream}");
        assert!(messages.iter().all(|m| m["role"] == "user"));
    }

    #[tokio::test]
    async fn auth_replaces_client_credentials_with_oauth_bearer() {
        let p = provider();
        let unified = p.request_out(request("{}")).expect("out");
        let mut req = p.request_in(unified).expect("in");
        req.headers
            .insert(X_API_KEY, HeaderValue::from_static("client-key"));
        req.headers
            .insert(AUTHORIZATION, HeaderValue::from_static("Bearer client"));
        p.auth(
            &mut req,
            &AccountCredential::Oauth {
                account_uuid: "u".into(),
                access_token: "at-1".into(),
                refresh_token: "rt-1".into(),
                expires_at_ms: 0,
                tier: None,
                last_refresh_ms: None,
            },
        )
        .await
        .expect("auth");
        assert_eq!(req.headers.get(AUTHORIZATION).unwrap(), "Bearer at-1");
        assert!(req.headers.get(X_API_KEY).is_none());
    }

    // PROV-02: a codex credential must never authenticate against the
    // anthropic provider — reaching `inject_credential` with one is a routing
    // bug and is rejected as an Auth error (not silently passed through).
    #[test]
    fn inject_credential_rejects_codex_credential_with_auth_error() {
        let mut headers = HeaderMap::new();
        let err = inject_credential(
            &mut headers,
            &AccountCredential::Codex {
                account_id: "acct-1".into(),
                access_token: "at-1".into(),
                refresh_token: "rt-1".into(),
                expires_at_ms: 0,
                last_refresh_ms: None,
            },
        )
        .expect_err("codex credential must not auth against anthropic");
        assert!(
            matches!(err, ProviderError::Auth(_)),
            "expected Auth error, got {err:?}"
        );
        // No credential header was injected.
        assert!(headers.get(AUTHORIZATION).is_none());
        assert!(headers.get(X_API_KEY).is_none());
    }

    #[tokio::test]
    async fn auth_injects_api_key_for_apikey_accounts() {
        let p = provider();
        let unified = p.request_out(request("{}")).expect("out");
        let mut req = p.request_in(unified).expect("in");
        req.headers
            .insert(AUTHORIZATION, HeaderValue::from_static("Bearer client"));
        p.auth(
            &mut req,
            &AccountCredential::Apikey {
                api_key: "sk-ant-api03-k".into(),
            },
        )
        .await
        .expect("auth");
        assert_eq!(req.headers.get(X_API_KEY).unwrap(), "sk-ant-api03-k");
        assert!(req.headers.get(AUTHORIZATION).is_none());
    }
}
