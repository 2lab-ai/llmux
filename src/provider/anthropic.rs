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
/// claude ALIAS in `model` to its catalog id (and a superseded id such as
/// `claude-haiku-4-5-20251001` — but only on a body [`floats_superseded_ids`]
/// accepts), strip the Claude-Code-local `[1m]` context-window suffix from
/// `model`, strip foreign (unsigned) thinking blocks from `messages` (issue
/// #116), and give Claude Code's bare `quota` rate-limit probe the Claude Code
/// system block ([`shape_quota_probe`]). The two model steps run in that order
/// and compose (`opus` → `claude-opus-5-5[1m]` → `claude-opus-5-5`).
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
    // Decided on the body AS RECEIVED: the probe counts whether or not its
    // system block is injected below.
    let superseded_float = floats_superseded_ids(&value);
    let alias = resolve_client_alias(&mut value, superseded_float);
    let model = strip_client_context_suffix(&mut value);
    let thinking = strip_foreign_thinking(&mut value);
    let quota = shape_quota_probe(&mut value);
    if !(alias || model || thinking || quota) {
        return body;
    }
    match serde_json::to_vec(&value) {
        Ok(bytes) => bytes::Bytes::from(bytes),
        Err(_) => body,
    }
}

/// Should a superseded claude id ([`crate::catalog::SUPERSEDED_CLAUDE_IDS`],
/// the Haiku 4.5 ids) in this body be served by its family alias's owner
/// (Haiku 5.5)? True for Claude Code's bare `quota` probe (it gets the Claude
/// Code system block, [`shape_quota_probe`]) and for a body whose FIRST system
/// block (a plain-string system is one block) is gate-shaped: its text starts
/// with `x-anthropic-billing-header:`, or EQUALS — exactly, untrimmed — one of
/// the Claude Code / Agent SDK identity sentences
/// ([`crate::proxy::auto_classifier::MAIN`], `MAIN_IN_SDK`, `SDK_AGENT`).
///
/// Why conditional: live probes through the daemon on 2026-10-23
/// (`claude-haiku-5-5`, OAuth subscription pool, one plain user turn) got 200
/// exactly for those first blocks — each identity sentence alone, the SDK
/// sentence followed by a custom block, a billing-header block followed by an
/// identity sentence, a custom block or nothing — and 502 (upstream 429 on
/// every account) for no system, a custom system, a custom block before an
/// identity sentence, and an identity sentence with anything appended in the
/// same block (trailing space, extra instructions). Haiku 4.5 accepted the
/// same refused bodies (bare plain: 4.5 200 ×2, 5.5 502 ×2). So a bare or
/// custom-system client naming a 4.5 id keeps reaching Haiku 4.5.
///
/// Shared with `proxy::forward`, which evaluates it once on the same inbound
/// bytes so the activity record names the model that actually served them.
pub(crate) fn floats_superseded_ids(body: &serde_json::Value) -> bool {
    is_quota_probe(body) || first_system_text(body).is_some_and(is_gate_shaped_system)
}

/// [`floats_superseded_ids`] over raw body bytes (`false` for non-JSON).
pub(crate) fn body_floats_superseded_ids(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .is_ok_and(|value| floats_superseded_ids(&value))
}

/// The text of the first system block: the whole string for a plain-string
/// system, else the first array element's `text`.
fn first_system_text(body: &serde_json::Value) -> Option<&str> {
    match body.get("system")? {
        serde_json::Value::String(text) => Some(text),
        serde_json::Value::Array(blocks) => blocks.first()?.get("text")?.as_str(),
        _ => None,
    }
}

/// See [`floats_superseded_ids`]: a billing-header prefix, or one identity
/// sentence verbatim.
fn is_gate_shaped_system(text: &str) -> bool {
    use crate::proxy::auto_classifier::{MAIN, MAIN_IN_SDK, SDK_AGENT};
    text.starts_with("x-anthropic-billing-header:")
        || [MAIN, MAIN_IN_SDK, SDK_AGENT].contains(&text)
}

/// Rewrite a client-supplied model that is a curated claude ALIAS (`opus`,
/// `sonnet`, `haiku`, …) to its catalog id, so the alias `GET /models`
/// advertises is actually honored upstream: before this hook a bare `opus` was
/// forwarded verbatim and 404'd at api.anthropic.com. Mapping lives in
/// [`crate::catalog::CLAUDE_MODELS`] behind
/// [`crate::catalog::resolve_claude_alias`], so a new curated row carries its
/// aliases automatically and anything that is not an alias (a real id, a
/// foreign slug) passes through untouched — passthrough stays passthrough.
/// The one exception is a superseded id
/// ([`crate::catalog::SUPERSEDED_CLAUDE_IDS`], 2026-10-23: the Haiku 4.5 ids):
/// when `superseded_float` ([`floats_superseded_ids`]) holds it is rewritten
/// to its family alias's current owner; otherwise it passes through as before.
fn resolve_client_alias(value: &mut serde_json::Value, superseded_float: bool) -> bool {
    let Some(model) = value.get("model").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let resolved = match crate::catalog::resolve_claude_alias(model) {
        Some(id) => Some(id),
        None if superseded_float => crate::catalog::resolve_superseded_claude_id(model),
        None => None,
    };
    let Some(id) = resolved else {
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

/// Give Claude Code's per-session rate-limit probe — `max_tokens: 1`, exactly
/// ONE `user` message whose content is `"quota"` (trimmed, the same test as
/// `proxy::classify`), no tools, and no system text — the Claude Code system
/// block ([`crate::proxy::auto_classifier::MAIN`]) before it leaves llmux.
/// The content may be the plain string (the only pinned capture, 2026-07-15)
/// or an array of exactly one `text` block (optionally `cache_control`), the
/// two forms `classify::last_user_text` reads; raw-io does not record 502'd
/// requests, so the 2026-10-23 probe body itself was not captured.
///
/// Why: live probes through the daemon on 2026-10-23 against the OAuth
/// subscription pool showed `claude-haiku-5-5` answering 200 to plain,
/// thinking (31,999 budget / 32,000 max_tokens, and adaptive), temperature-1
/// and Agent-SDK-system requests and to a system-less `count_tokens`, but 502
/// (upstream `429 rate_limit_error` on every account) to exactly this probe
/// shape — while the same body on `claude-haiku-4-5-20251001` returned 200.
/// The ledger holds 137 such probes on `claude-haiku-5-5` that already 502'd
/// (2026-10-06..2026-10-23), and floating the explicit Haiku 4.5 ids onto
/// Haiku 5.5 (`crate::catalog::SUPERSEDED_CLAUDE_IDS`) would route every
/// built-in probe there. The same probe body plus this system block returned
/// 200 on `claude-haiku-5-5` (4 of 4, string and single-text-block content;
/// the bare control in the same run 502'd).
///
/// Deliberately narrow: only this probe is shaped. An arbitrary bare user
/// request (a different text, a second turn, tools, any non-blank system, any
/// other `max_tokens`) passes through untouched — llmux does not otherwise
/// shape claude requests. Classification and internal-task routing read the
/// ORIGINAL body at forward entry, so the injected block cannot change how
/// the probe itself is classified.
fn shape_quota_probe(value: &mut serde_json::Value) -> bool {
    if !is_quota_probe(value) {
        return false;
    }
    value["system"] = serde_json::json!([
        {"type": "text", "text": crate::proxy::auto_classifier::MAIN}
    ]);
    tracing::debug!("added the Claude Code system block to a bare quota probe");
    true
}

fn is_quota_probe(value: &serde_json::Value) -> bool {
    use serde_json::Value;
    let Some(body) = value.as_object() else {
        return false;
    };
    if body.get("max_tokens").and_then(Value::as_u64) != Some(1) {
        return false;
    }
    let one_quota_turn = body
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| match messages.as_slice() {
            [only] => {
                only.get("role").and_then(Value::as_str) == Some("user")
                    && only.get("content").is_some_and(is_quota_content)
            }
            _ => false,
        });
    let no_tools = match body.get("tools") {
        None | Some(Value::Null) => true,
        Some(Value::Array(tools)) => tools.is_empty(),
        Some(_) => false,
    };
    let blank_system = match body.get("system") {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.trim().is_empty(),
        Some(Value::Array(blocks)) => blocks.iter().all(|block| {
            block.get("type").and_then(Value::as_str) == Some("text")
                && block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.trim().is_empty())
        }),
        Some(_) => false,
    };
    one_quota_turn && no_tools && blank_system
}

/// `"quota"` as a plain string, or as exactly one `text` block (other block
/// fields such as `cache_control` are ignored). Any other block type, or more
/// than one block, is not the probe.
fn is_quota_content(content: &serde_json::Value) -> bool {
    use serde_json::Value;
    let is_quota = |text: &str| text.trim() == "quota";
    match content {
        Value::String(text) => is_quota(text),
        Value::Array(blocks) => match blocks.as_slice() {
            [block] => {
                block.get("type").and_then(Value::as_str) == Some("text")
                    && block
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(is_quota)
            }
            _ => false,
        },
        _ => false,
    }
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
        AccountCredential::Gemini { .. } => {
            return Err(ProviderError::Auth(
                "gemini credential cannot authenticate against the anthropic provider".into(),
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

    /// Normalize the outbound `model`, strip foreign (unsigned) thinking
    /// blocks (issue #116) and shape the bare `quota` probe in one parse
    /// (`normalize_body`); otherwise unwrap without reserializing (moves the
    /// original wire body out).
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
        assert_eq!(normalized_model("opus"), "claude-opus-5-5");
        assert_eq!(normalized_model("opus-5-5"), "claude-opus-5-5");
        assert_eq!(normalized_model("opus-5"), "claude-opus-5");
        assert_eq!(normalized_model("sonnet"), "claude-sonnet-5-5");
        assert_eq!(normalized_model("sonnet-5-5"), "claude-sonnet-5-5");
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
        assert_eq!(normalized_model("opus[1m]"), "claude-opus-5-5");
    }

    /// Floating Haiku aliases use the new wire id whatever the body shape.
    #[test]
    fn normalize_body_resolves_haiku_5_5_aliases() {
        for model in ["haiku", "haiku[1m]", "haiku-5-5", "claude-haiku-5-5[1m]"] {
            assert_eq!(normalized_model(model), "claude-haiku-5-5", "{model}");
        }
    }

    /// A one-turn body naming `model` with the given `system` (omitted when
    /// `None`).
    fn plain_body(model: &str, system: Option<serde_json::Value>) -> String {
        let mut body = serde_json::json!({
            "model": model,
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hi"}],
        });
        if let Some(system) = system {
            body["system"] = system;
        }
        body.to_string()
    }

    /// User decision 2026-10-23: the explicit Haiku 4.5 ids Claude Code / the
    /// Agent SDK send for their built-in Haiku tier float to the `haiku` owner
    /// — but only on a gate-shaped body (Haiku 5.5 on the subscription
    /// endpoint refuses the rest, live probes 2026-10-23).
    #[test]
    fn normalize_body_floats_a_4_5_id_only_on_a_gate_shaped_body() {
        let cc = serde_json::json!([
            {"type": "text", "text": "x-anthropic-billing-header: cc_version=2.1.292; cc_entrypoint=cli;"},
            {"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."},
        ]);
        let billing_only = serde_json::json!([
            {"type": "text", "text": "x-anthropic-billing-header: cc_version=9.9.9;"},
            {"type": "text", "text": "You are a terse assistant."},
        ]);
        let sdk =
            serde_json::json!("You are a Claude agent, built on Anthropic's Claude Agent SDK.");
        for model in ["claude-haiku-4-5", "claude-haiku-4-5-20251001"] {
            for system in [&cc, &billing_only, &sdk] {
                let out =
                    normalize_body(bytes::Bytes::from(plain_body(model, Some(system.clone()))));
                let value: serde_json::Value = serde_json::from_slice(&out).expect("json");
                assert_eq!(value["model"], "claude-haiku-5-5", "{model} / {system}");
                assert_eq!(&value["system"], system, "system untouched");
            }
        }
        // Bare and custom-system 4.5 requests keep reaching Haiku 4.5: the
        // body comes back byte-identical.
        let custom = [
            None,
            Some(serde_json::json!("You are a terse assistant.")),
            Some(serde_json::json!([
                {"type": "text", "text": "You are a terse assistant."},
                {"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."},
            ])),
        ];
        for system in custom {
            let body = plain_body("claude-haiku-4-5-20251001", system.clone());
            let out = normalize_body(bytes::Bytes::from(body.clone()));
            assert_eq!(out.as_ref(), body.as_bytes(), "{system:?}");
        }
    }

    /// The predicate mirrors the 2026-10-23 live probe table on
    /// `claude-haiku-5-5` (200 = floats, 502 = does not).
    #[test]
    fn floats_superseded_ids_matches_the_measured_first_system_blocks() {
        const CC: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
        const CC_IN_SDK: &str =
            "You are Claude Code, Anthropic's official CLI for Claude, running within the Claude Agent SDK.";
        const SDK: &str = "You are a Claude agent, built on Anthropic's Claude Agent SDK.";
        const HEADER: &str = "x-anthropic-billing-header: cc_version=9.9.9; cc_entrypoint=cli;";
        const CUSTOM: &str = "You are a terse assistant.";
        let block = |text: &str| serde_json::json!({"type": "text", "text": text});
        let with_system = |system: Option<serde_json::Value>| -> serde_json::Value {
            serde_json::from_str(&plain_body("claude-haiku-5-5", system)).expect("json")
        };
        let accepted = [
            serde_json::json!([block(CC)]),
            serde_json::json!(CC),
            serde_json::json!([block(CC_IN_SDK)]),
            serde_json::json!([block(SDK)]),
            serde_json::json!([block(SDK), block(CUSTOM)]),
            serde_json::json!([block(HEADER), block(SDK)]),
            serde_json::json!([block(HEADER), block(CC)]),
            serde_json::json!([block(HEADER), block(CC_IN_SDK)]),
            serde_json::json!([block(HEADER), block(CUSTOM)]),
            serde_json::json!([block(HEADER)]),
        ];
        for system in accepted {
            assert!(
                floats_superseded_ids(&with_system(Some(system.clone()))),
                "200 shape must float: {system}"
            );
        }
        let refused = [
            None,
            Some(serde_json::json!(CUSTOM)),
            Some(serde_json::json!([block(CUSTOM), block(CC)])),
            Some(serde_json::json!([block(CUSTOM), block(SDK)])),
            Some(serde_json::json!([block(&format!(
                "{CC} Extra instructions follow."
            ))])),
            Some(serde_json::json!([block(&format!("{CC} "))])),
            Some(serde_json::json!(format!(
                "{CC}\n\nYou are a terse assistant."
            ))),
        ];
        for system in refused {
            assert!(
                !floats_superseded_ids(&with_system(system.clone())),
                "502 shape must not float: {system:?}"
            );
        }
        // The bare quota probe floats (it gets the Claude Code block).
        let probe = serde_json::json!({
            "model": "claude-haiku-4-5", "max_tokens": 1,
            "messages": [{"role": "user", "content": "quota"}],
        });
        assert!(floats_superseded_ids(&probe));
        assert!(body_floats_superseded_ids(probe.to_string().as_bytes()));
        assert!(!body_floats_superseded_ids(b"not json"));
    }

    #[test]
    fn normalize_body_resolves_aliases_case_insensitively_and_trimmed() {
        assert_eq!(normalized_model("  OPUS  "), "claude-opus-5-5");
    }

    /// A real id is not an alias: only the pre-existing suffix strip applies.
    #[test]
    fn normalize_body_leaves_a_real_id_to_the_suffix_strip() {
        assert_eq!(normalized_model("claude-opus-5[1m]"), "claude-opus-5");
        assert_eq!(normalized_model("claude-opus-5-5[1m]"), "claude-opus-5-5");
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

    /// Claude Code's rate-limit probe (sanitized 2.1.292 capture shape, the
    /// same one `tests/e2e.rs::internal_quota` uses) with `extra` merged in.
    fn quota_probe(extra: serde_json::Value) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": "claude-haiku-5-5",
            "max_tokens": 1,
            "metadata": {"user_id": "session"},
            "messages": [{"role": "user", "content": "quota"}],
        });
        if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                body.insert(key.clone(), value.clone());
            }
        }
        body
    }

    fn normalized(body: &serde_json::Value) -> bytes::Bytes {
        normalize_body(bytes::Bytes::from(body.to_string()))
    }

    fn claude_code_system() -> serde_json::Value {
        serde_json::json!([{
            "type": "text",
            "text": "You are Claude Code, Anthropic's official CLI for Claude."
        }])
    }

    /// 2026-10-23: the system-less probe 502'd on `claude-haiku-5-5` (upstream
    /// 429 on every account). Absent / `null` / `""` / `[]` / blank-text
    /// systems all count as "no system" and get the Claude Code block.
    #[test]
    fn normalize_body_gives_the_bare_quota_probe_the_claude_code_system() {
        let blank_systems = [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!("")),
            Some(serde_json::json!([])),
            Some(serde_json::json!("  \n")),
            Some(serde_json::json!([{"type": "text", "text": " "}])),
        ];
        for system in blank_systems {
            let extra = match &system {
                Some(system) => serde_json::json!({"system": system}),
                None => serde_json::json!({}),
            };
            let out: serde_json::Value =
                serde_json::from_slice(&normalized(&quota_probe(extra))).expect("json");
            assert_eq!(out["system"], claude_code_system(), "system {system:?}");
            // Nothing else about the probe changes.
            assert_eq!(out["max_tokens"], 1);
            assert_eq!(
                out["messages"],
                serde_json::json!([{"role": "user", "content": "quota"}])
            );
            assert_eq!(out["model"], "claude-haiku-5-5");
        }
        // Trimmed equality on the text, like `proxy::classify`; `tools: []`
        // and `tools: null` are "no tools". The content may also be ONE text
        // block (optionally cache-controlled) — the other form
        // `classify::last_user_text` reads.
        for extra in [
            serde_json::json!({"messages": [{"role": "user", "content": " quota\n"}]}),
            serde_json::json!({"tools": []}),
            serde_json::json!({"tools": null}),
            serde_json::json!({"messages": [{"role": "user", "content": [
                {"type": "text", "text": "quota"}
            ]}]}),
            serde_json::json!({"messages": [{"role": "user", "content": [
                {"type": "text", "text": " quota ", "cache_control": {"type": "ephemeral"}}
            ]}]}),
        ] {
            let out: serde_json::Value =
                serde_json::from_slice(&normalized(&quota_probe(extra.clone()))).expect("json");
            assert_eq!(out["system"], claude_code_system(), "{extra}");
        }
    }

    /// Narrow by design: anything that is not exactly the probe passes through
    /// byte-identical (refcounted fast path) — llmux does not shape arbitrary
    /// bare user requests.
    #[test]
    fn normalize_body_leaves_every_near_miss_of_the_quota_probe_untouched() {
        for extra in [
            serde_json::json!({"max_tokens": 2}),
            serde_json::json!({"max_tokens": null}),
            serde_json::json!({"messages": [{"role": "user", "content": "quota please"}]}),
            serde_json::json!({"messages": [{"role": "user", "content": "Quota"}]}),
            // Block form: two blocks, a non-text block, no block, other text.
            serde_json::json!({"messages": [{"role": "user", "content": [
                {"type": "text", "text": "quota"},
                {"type": "text", "text": "quota"}
            ]}]}),
            serde_json::json!({"messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "x"}}
            ]}]}),
            serde_json::json!({"messages": [{"role": "user", "content": []}]}),
            serde_json::json!({"messages": [{"role": "user", "content": [
                {"type": "text", "text": "quota please"}
            ]}]}),
            serde_json::json!({"messages": [{"role": "assistant", "content": "quota"}]}),
            serde_json::json!({"messages": [
                {"role": "user", "content": "quota"},
                {"role": "user", "content": "quota"}
            ]}),
            serde_json::json!({"tools": [{"name": "t", "input_schema": {"type": "object"}}]}),
            serde_json::json!({"system": "You are a helpful assistant."}),
            serde_json::json!({"system": [{"type": "text", "text": "x-anthropic-billing-header: cc_version=2.1.292;"}]}),
            serde_json::json!({"system": [{"type": "image"}]}),
        ] {
            let body = quota_probe(extra.clone()).to_string();
            let out = normalize_body(bytes::Bytes::from(body.clone()));
            assert_eq!(out.as_ref(), body.as_bytes(), "{extra}");
        }
        // No `max_tokens` at all is not the probe either.
        let mut body = quota_probe(serde_json::json!({}));
        body.as_object_mut().expect("object").remove("max_tokens");
        let body = body.to_string();
        let out = normalize_body(bytes::Bytes::from(body.clone()));
        assert_eq!(out.as_ref(), body.as_bytes());
    }

    /// The steps compose in one parse: Claude Code's built-in probe on the
    /// explicit Haiku 4.5 id comes out on Haiku 5.5 AND with the system block.
    #[test]
    fn normalize_body_floats_and_shapes_a_haiku_4_5_quota_probe() {
        let probe = quota_probe(serde_json::json!({"model": "claude-haiku-4-5-20251001"}));
        let out: serde_json::Value = serde_json::from_slice(&normalized(&probe)).expect("json");
        assert_eq!(out["model"], "claude-haiku-5-5");
        assert_eq!(out["system"], claude_code_system());
        assert_eq!(out["metadata"]["user_id"], "session");
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
