//! `OpenRouterProvider` — the `openrouter` backend group's transformer
//! (docs/openrouter/spec.md §R3/§R4).
//!
//! OpenRouter exposes a NATIVE Anthropic Messages endpoint
//! (`POST https://openrouter.ai/api/v1/messages`, live-probed 2026-08-21: it
//! answers with the Anthropic error envelope, not OpenAI's), so this provider
//! is shaped like [`super::anthropic::AnthropicPassthrough`] — a passthrough —
//! and NOT like the codex/grok Messages↔Responses translators. No SSE
//! converter, no format translation. The only body mutation is the `model`
//! field rewrite (`or-<name>` → the OpenRouter slug), plus request-header
//! hygiene for the Claude-Code-specific beta flags OpenRouter does not know.

use http::header::AUTHORIZATION;
use http::{HeaderMap, HeaderName, HeaderValue};

use super::{
    AnthropicRequest, AnthropicResponse, Provider, ProviderError, ProviderRequest,
    ProviderResponse, UnifiedRequest, UnifiedResponse,
};
use crate::config::AccountCredential;

/// Free-model pin the bare `or` family alias resolves to when the config
/// carries no explicit `openrouter.default_model`.
///
/// Must stay in sync with `config::schema::default_openrouter_model` (that
/// function's doc comment points back here); it is duplicated rather than
/// imported because the schema default is a private serde helper.
pub const OPENROUTER_DEFAULT_MODEL: &str = "stealth/ox-alpha";

/// Client auth header stripped on the way upstream (OpenRouter accepts BOTH
/// `x-api-key` and `Authorization: Bearer`, so a client-supplied `x-api-key`
/// would otherwise be a live credential leak into someone else's account).
const X_API_KEY: HeaderName = HeaderName::from_static("x-api-key");

/// Claude-Code-local beta flags. `anthropic-beta` carries Anthropic-only
/// feature gates (oauth, 1M context, fine-grained tool streaming) and
/// `anthropic-dangerous-direct-browser-access` is a CORS opt-in for
/// api.anthropic.com; OpenRouter knows neither. `anthropic-version` is the
/// wire-format version of the payload itself and IS kept.
const ANTHROPIC_BETA: HeaderName = HeaderName::from_static("anthropic-beta");
const ANTHROPIC_DIRECT_BROWSER: HeaderName =
    HeaderName::from_static("anthropic-dangerous-direct-browser-access");

/// Resolve a client-supplied model to the slug OpenRouter should serve
/// (spec §R3). `pin` is the live free-model pin (`config.openrouter
/// .default_model`, default [`OPENROUTER_DEFAULT_MODEL`]).
///
/// Order, exactly as specified:
/// 1. No model at all → the pin.
/// 2. Trim + ASCII-lowercase, then strip ONE trailing `[1m]` — the same
///    client-metadata convention `routing::Classifier::classify` strips, so a
///    model classifies and resolves identically with or without the suffix.
/// 3. Bare `or` → the pin (mirrors bare `grok` → the grok pin).
/// 4. Strip a leading `or-`. When nothing was stripped and the string already
///    contains `/`, it is a bare OpenRouter slug (`openrouter/free`) and rides
///    through verbatim.
/// 5. A remainder containing `/` is used VERBATIM
///    (`or-openai/gpt-oss-20b:free` → `openai/gpt-oss-20b:free`) — the escape
///    hatch for the ~400 models the curated table does not carry.
/// 6. Otherwise the curated alias table ([`crate::catalog::OPENROUTER_MODELS`]
///    behind [`crate::catalog::resolve_openrouter_alias`], keyed by the FULL
///    advertised id including the `or-` prefix) maps `or-ox-alpha` →
///    `stealth/ox-alpha`.
/// 7. No match → the remainder verbatim, so OpenRouter's own 404 reaches the
///    user. A curated set is a convenience layer, never a gate, and silently
///    substituting a DIFFERENT model would bill/answer from something the user
///    did not ask for.
///
/// Edge case not covered by the spec text: an empty request (`Some("")`, or a
/// dangling `or-`) names no model at all, so it is treated like `None` and
/// yields the pin — emitting `"model": ""` upstream would be a 400 with a
/// worse message than the one the user asked for.
pub fn resolve_model(requested: Option<&str>, pin: &str) -> String {
    let Some(requested) = requested else {
        return pin.to_string();
    };
    let lowered = requested.trim().to_ascii_lowercase();
    let lowered = lowered
        .strip_suffix("[1m]")
        .map(str::to_string)
        .unwrap_or(lowered);
    if lowered.is_empty() || lowered == "or" {
        return pin.to_string();
    }
    // `remainder` is what goes upstream when no curated alias matches; it is
    // the input minus the `or-` selector prefix (or the input itself when the
    // client already spelled a bare OpenRouter slug).
    let remainder = lowered.strip_prefix("or-").unwrap_or(&lowered);
    if remainder.is_empty() {
        return pin.to_string();
    }
    if remainder.contains('/') {
        return remainder.to_string();
    }
    if let Some(slug) = crate::catalog::resolve_openrouter_alias(&lowered) {
        return slug.to_string();
    }
    remainder.to_string()
}

/// Rewrite the JSON body's `model` field to the resolved OpenRouter slug in
/// ONE parse. The original (refcounted) bytes come back unchanged when the
/// resolution is a no-op, and a non-JSON — or non-object — body passes through
/// untouched: passthrough never fails on body shape.
///
/// A body with no `model` at all gets the pin inserted, because that is what
/// [`resolve_model`] answers for `None` and OpenRouter (like Anthropic)
/// requires the field.
fn rewrite_model(body: bytes::Bytes, pin: &str) -> bytes::Bytes {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let Some(object) = value.as_object_mut() else {
        return body;
    };
    let requested = object
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let resolved = resolve_model(requested.as_deref(), pin);
    if requested.as_deref() == Some(resolved.as_str()) {
        return body;
    }
    object.insert("model".to_string(), serde_json::Value::String(resolved));
    match serde_json::to_vec(&value) {
        Ok(bytes) => bytes::Bytes::from(bytes),
        Err(_) => body,
    }
}

/// Strip the client's auth headers and inject the account's OpenRouter key as
/// `Authorization: Bearer sk-or-v1-…` (both header shapes are accepted
/// upstream; Bearer is the documented one). Any OTHER credential variant
/// reaching here is a routing bug — the proxy selects the provider by backend
/// group before this point — and is an `Auth` error rather than a silent
/// pass-through of the client's own auth (mirrors
/// `provider::anthropic::inject_credential`'s cross-provider guards). The
/// match is exhaustive on purpose: a future credential variant must be a
/// compile error here, not a default-arm leak.
fn inject_credential(
    headers: &mut HeaderMap,
    credential: &AccountCredential,
) -> Result<(), ProviderError> {
    headers.remove(X_API_KEY);
    headers.remove(AUTHORIZATION);
    match credential {
        AccountCredential::OpenRouter { api_key, .. } => {
            let value = HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|err| ProviderError::Auth(err.to_string()))?;
            headers.insert(AUTHORIZATION, value);
        }
        AccountCredential::Oauth { .. } => {
            return Err(ProviderError::Auth(
                "oauth credential cannot authenticate against the openrouter provider".into(),
            ));
        }
        AccountCredential::Apikey { .. } => {
            return Err(ProviderError::Auth(
                "apikey credential cannot authenticate against the openrouter provider".into(),
            ));
        }
        AccountCredential::Codex { .. } => {
            return Err(ProviderError::Auth(
                "codex credential cannot authenticate against the openrouter provider".into(),
            ));
        }
        AccountCredential::Grok { .. } => {
            return Err(ProviderError::Auth(
                "grok credential cannot authenticate against the openrouter provider".into(),
            ));
        }
    }
    Ok(())
}

/// Passthrough transformer for OpenRouter's Anthropic Messages endpoint.
#[derive(Debug, Clone)]
pub struct OpenRouterProvider {
    /// Upstream base URL (config `openrouter.upstream`, default
    /// `https://openrouter.ai/api/v1`).
    base_url: String,
    /// The free-model pin a bare `or` (or a model-less body) resolves to
    /// (config `openrouter.default_model`).
    default_model: String,
}

impl OpenRouterProvider {
    pub fn new(base_url: impl Into<String>, default_model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            default_model: default_model.into(),
        }
    }

    /// The pin — the slug a bare `or` resolves to. Read by the `/models`
    /// catalog so the advertised `or` family alias names the live pin.
    pub fn model(&self) -> &str {
        &self.default_model
    }

    /// Upstream base URL (inherent twin of [`Provider::endpoint`], so callers
    /// holding the concrete type need no trait import).
    pub fn endpoint(&self) -> &str {
        &self.base_url
    }
}

impl Provider for OpenRouterProvider {
    fn name(&self) -> &'static str {
        "openrouter"
    }

    fn endpoint(&self) -> &str {
        &self.base_url
    }

    /// Strip client-supplied `x-api-key` / `authorization`, inject the
    /// account's OpenRouter key as a Bearer token.
    async fn auth(
        &self,
        req: &mut ProviderRequest,
        account: &AccountCredential,
    ) -> Result<(), ProviderError> {
        inject_credential(&mut req.headers, account)
    }

    /// Identity wrap. Extracts `model` and `stream` from the JSON body when
    /// present without touching the body bytes; a non-JSON body simply yields
    /// no flags. `model` extraction is shared with the proxy's routing path
    /// (`routing::model_from_body`); `stream` stays local — same as the
    /// Anthropic passthrough.
    fn request_out(
        &self,
        anthropic_req: AnthropicRequest,
    ) -> Result<UnifiedRequest, ProviderError> {
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

    /// Resolve `model` to an OpenRouter slug (§R3, one parse) and drop the
    /// Claude-Code-specific beta headers (§R4); everything else — including
    /// `anthropic-version` — rides through untouched.
    fn request_in(&self, unified: UnifiedRequest) -> Result<ProviderRequest, ProviderError> {
        let wire = unified.wire;
        let mut headers = wire.headers;
        headers.remove(ANTHROPIC_BETA);
        headers.remove(ANTHROPIC_DIRECT_BROWSER);
        Ok(ProviderRequest {
            method: wire.method,
            path: wire.path,
            headers,
            body: rewrite_model(wire.body, &self.default_model),
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
    /// The config default + the client's verbatim path must compose to the
    /// REAL OpenRouter Messages endpoint. This is a silent-total-failure
    /// guard, not a style check: `forward.rs::send_upstream` builds the URL as
    /// `format!("{endpoint}{path}")` with `path` = the client's `/v1/messages`,
    /// so an `endpoint` of `…/api/v1` composes `…/api/v1/v1/messages`. Live
    /// probes 2026-08-21: the wrong URL → 404, the right one → 401 (auth, i.e.
    /// the route exists). Every openrouter request would have 404'd.
    #[test]
    fn openrouter_upstream_composes_the_real_endpoint() {
        let endpoint = crate::config::OpenRouterConfig::default().upstream;
        let composed = format!("{}{}", endpoint.trim_end_matches('/'), "/v1/messages");
        assert_eq!(composed, "https://openrouter.ai/api/v1/messages");
        // And the auth constants stay absolute (they do NOT ride `upstream`).
        assert_eq!(
            crate::auth::openrouter::KEYS_URL,
            "https://openrouter.ai/api/v1/auth/keys"
        );
    }

    use super::*;
    use http::Method;

    const PIN: &str = OPENROUTER_DEFAULT_MODEL;

    fn provider() -> OpenRouterProvider {
        OpenRouterProvider::new("https://openrouter.ai/api/v1", PIN)
    }

    fn request(body: &str) -> AnthropicRequest {
        AnthropicRequest {
            method: Method::POST,
            path: "/v1/messages".to_string(),
            headers: HeaderMap::new(),
            body: bytes::Bytes::copy_from_slice(body.as_bytes()),
        }
    }

    /// The upstream `model` a client-supplied model actually turns into.
    fn upstream_model(model: &str) -> String {
        resolve_model(Some(model), PIN)
    }

    // ---- resolve_model (§R3) ----

    #[test]
    fn no_model_resolves_to_the_pin() {
        assert_eq!(resolve_model(None, PIN), "stealth/ox-alpha");
        assert_eq!(
            resolve_model(None, "z-ai/glm-5.2:free"),
            "z-ai/glm-5.2:free"
        );
    }

    #[test]
    fn bare_or_resolves_to_the_pin() {
        assert_eq!(upstream_model("or"), "stealth/ox-alpha");
        assert_eq!(
            resolve_model(Some("or"), "openrouter/free"),
            "openrouter/free"
        );
    }

    #[test]
    fn curated_alias_resolves_to_its_slug() {
        assert_eq!(upstream_model("or-ox-alpha"), "stealth/ox-alpha");
        assert_eq!(upstream_model("or-glm-5.2"), "z-ai/glm-5.2:free");
        assert_eq!(upstream_model("or-free"), "openrouter/free");
    }

    /// The escape hatch for the ~400 non-curated models: anything with a `/`
    /// is used verbatim after the `or-` selector is removed.
    #[test]
    fn slug_after_the_prefix_passes_through_verbatim() {
        assert_eq!(
            upstream_model("or-openai/gpt-oss-20b:free"),
            "openai/gpt-oss-20b:free"
        );
    }

    /// A bare OpenRouter slug (the `Prefix("openrouter/")` routing rule) has
    /// no `or-` to strip and must not be mangled.
    #[test]
    fn bare_openrouter_slug_passes_through_verbatim() {
        assert_eq!(upstream_model("openrouter/free"), "openrouter/free");
    }

    /// Never silently substitute a different model — OpenRouter's own 404
    /// must reach the user.
    #[test]
    fn unknown_alias_passes_through_verbatim_without_substitution() {
        let out = upstream_model("or-nonexistent");
        assert_eq!(out, "nonexistent");
        assert_ne!(out, PIN, "an unknown alias must NOT fall back to the pin");
    }

    #[test]
    fn resolution_is_case_insensitive_and_trimmed() {
        assert_eq!(upstream_model("OR-Ox-Alpha "), "stealth/ox-alpha");
        assert_eq!(upstream_model("  OR  "), "stealth/ox-alpha");
    }

    /// `[1m]` is Claude-Code display metadata (the same suffix
    /// `routing::Classifier::classify` strips), never part of the slug.
    #[test]
    fn client_context_suffix_is_stripped_before_resolution() {
        assert_eq!(upstream_model("or-ox-alpha[1m]"), "stealth/ox-alpha");
        assert_eq!(upstream_model("or[1m]"), "stealth/ox-alpha");
        assert_eq!(
            upstream_model("or-openai/gpt-oss-20b:free[1m]"),
            "openai/gpt-oss-20b:free"
        );
    }

    /// An empty (or selector-only) model names nothing — treated like `None`
    /// rather than sending `"model": ""` upstream.
    #[test]
    fn empty_model_resolves_to_the_pin() {
        assert_eq!(upstream_model(""), "stealth/ox-alpha");
        assert_eq!(upstream_model("  "), "stealth/ox-alpha");
        assert_eq!(upstream_model("or-"), "stealth/ox-alpha");
    }

    // ---- request_out / request_in ----

    #[test]
    fn request_out_extracts_model_and_stream() {
        let unified = provider()
            .request_out(request(
                r#"{"model":"or-ox-alpha","stream":true,"messages":[]}"#,
            ))
            .expect("unified");
        assert_eq!(unified.model.as_deref(), Some("or-ox-alpha"));
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
    fn request_in_rewrites_the_body_model_and_keeps_everything_else() {
        let body =
            r#"{"model":"or-ox-alpha","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#;
        let p = provider();
        let unified = p.request_out(request(body)).expect("out");
        let provider_req = p.request_in(unified).expect("in");
        let upstream: serde_json::Value =
            serde_json::from_slice(&provider_req.body).expect("upstream json");
        assert_eq!(upstream["model"], "stealth/ox-alpha");
        assert_eq!(upstream["max_tokens"], 8);
        assert_eq!(upstream["messages"][0]["content"], "hi");
        assert_eq!(provider_req.path, "/v1/messages");
        assert_eq!(provider_req.method, Method::POST);
    }

    /// Header hygiene: the Claude-Code beta flags go, `anthropic-version` and
    /// unrelated headers stay.
    #[test]
    fn request_in_drops_claude_code_beta_headers_and_keeps_the_version() {
        let p = provider();
        let mut req = request(r#"{"model":"or","messages":[]}"#);
        req.headers.insert(
            ANTHROPIC_BETA,
            HeaderValue::from_static("oauth-2025-04-20,context-1m-2025-08-07"),
        );
        req.headers
            .insert(ANTHROPIC_DIRECT_BROWSER, HeaderValue::from_static("true"));
        req.headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
        req.headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );
        let unified = p.request_out(req).expect("out");
        let provider_req = p.request_in(unified).expect("in");
        assert!(provider_req.headers.get(ANTHROPIC_BETA).is_none());
        assert!(provider_req.headers.get(ANTHROPIC_DIRECT_BROWSER).is_none());
        assert_eq!(
            provider_req.headers.get("anthropic-version").expect("kept"),
            "2023-06-01"
        );
        assert_eq!(
            provider_req.headers.get("content-type").expect("kept"),
            "application/json"
        );
    }

    /// A body whose model already IS the resolved slug changes nothing — the
    /// refcounted byte-identity fast path.
    #[test]
    fn request_in_is_byte_identical_when_the_model_is_already_resolved() {
        let body = r#"{"model":"stealth/ox-alpha","messages":[]}"#;
        let out = rewrite_model(bytes::Bytes::from_static(body.as_bytes()), PIN);
        assert_eq!(out.as_ref(), body.as_bytes());
    }

    #[test]
    fn request_in_passes_a_non_json_body_through_untouched() {
        let p = provider();
        let unified = p.request_out(request("not json")).expect("out");
        let provider_req = p.request_in(unified).expect("in");
        assert_eq!(provider_req.body.as_ref(), b"not json");
    }

    /// A JSON body that is not an object has no `model` field to rewrite and
    /// must not panic on indexed assignment.
    #[test]
    fn request_in_passes_a_non_object_json_body_through_untouched() {
        let out = rewrite_model(bytes::Bytes::from_static(b"[1,2,3]"), PIN);
        assert_eq!(out.as_ref(), b"[1,2,3]");
    }

    /// A model-less object gets the pin, mirroring `resolve_model(None, pin)`
    /// — OpenRouter requires the field.
    #[test]
    fn request_in_inserts_the_pin_when_the_body_has_no_model() {
        let out = rewrite_model(bytes::Bytes::from_static(br#"{"messages":[]}"#), PIN);
        let upstream: serde_json::Value = serde_json::from_slice(&out).expect("json");
        assert_eq!(upstream["model"], "stealth/ox-alpha");
    }

    // ---- auth (§R4) ----

    #[tokio::test]
    async fn auth_injects_bearer_and_strips_client_credentials() {
        let p = provider();
        let unified = p.request_out(request("{}")).expect("out");
        let mut req = p.request_in(unified).expect("in");
        req.headers
            .insert(X_API_KEY, HeaderValue::from_static("client-key"));
        req.headers
            .insert(AUTHORIZATION, HeaderValue::from_static("Bearer client"));
        p.auth(
            &mut req,
            &AccountCredential::OpenRouter {
                api_key: "sk-or-v1-abc".into(),
                label: "llmux".into(),
            },
        )
        .await
        .expect("auth");
        assert_eq!(
            req.headers.get(AUTHORIZATION).expect("bearer"),
            "Bearer sk-or-v1-abc"
        );
        assert!(req.headers.get(X_API_KEY).is_none());
    }

    /// Every foreign credential is an Auth error and leaves NO credential
    /// header behind (a routing bug must fail loudly, never leak the client's
    /// own auth upstream).
    #[test]
    fn auth_rejects_every_foreign_credential() {
        let foreign = [
            AccountCredential::Oauth {
                account_uuid: "u".into(),
                access_token: "at-1".into(),
                refresh_token: "rt-1".into(),
                expires_at_ms: 0,
                tier: None,
                last_refresh_ms: None,
            },
            AccountCredential::Apikey {
                api_key: "sk-ant-api03-k".into(),
            },
            AccountCredential::Codex {
                account_id: "acct-1".into(),
                access_token: "at-1".into(),
                refresh_token: "rt-1".into(),
                expires_at_ms: 0,
                last_refresh_ms: None,
            },
            AccountCredential::Grok {
                subject: "s".into(),
                access_token: "at-1".into(),
                refresh_token: "rt-1".into(),
                expires_at_ms: 0,
                token_endpoint: String::new(),
                last_refresh_ms: None,
            },
        ];
        for credential in &foreign {
            let mut headers = HeaderMap::new();
            headers.insert(X_API_KEY, HeaderValue::from_static("client-key"));
            let err = inject_credential(&mut headers, credential)
                .expect_err("foreign credential must not auth against openrouter");
            let ProviderError::Auth(message) = &err else {
                panic!("expected Auth error, got {err:?}");
            };
            assert!(
                message.contains(credential.kind()) && message.contains("openrouter"),
                "message names the kind and the provider: {message}"
            );
            assert!(headers.get(AUTHORIZATION).is_none());
            assert!(headers.get(X_API_KEY).is_none(), "client key stripped");
        }
    }

    #[test]
    fn provider_identity_and_accessors() {
        let p = OpenRouterProvider::new("https://openrouter.ai/api/v1", "openrouter/free");
        assert_eq!(p.name(), "openrouter");
        assert_eq!(p.endpoint(), "https://openrouter.ai/api/v1");
        assert_eq!(p.model(), "openrouter/free");
        assert_eq!(Provider::endpoint(&p), "https://openrouter.ai/api/v1");
    }

    // ---- response hooks ----

    #[test]
    fn response_hooks_are_identity() {
        let p = provider();
        let unified = p
            .response_in(ProviderResponse {
                status: http::StatusCode::OK,
                headers: HeaderMap::new(),
                body: bytes::Bytes::from_static(b"{\"type\":\"message\"}"),
            })
            .expect("response_in");
        let out = p.response_out(unified).expect("response_out");
        assert_eq!(out.status, http::StatusCode::OK);
        assert_eq!(out.body.as_ref(), b"{\"type\":\"message\"}");
    }
}
