//! Provider abstraction (FR4): a transformer trait modeled on
//! claude-code-router's hourglass — Anthropic-shaped wire format in, a
//! unified intermediate at the waist, provider-native format out, and back.
//!
//! Shipping: [`anthropic::AnthropicPassthrough`] (all hooks identity,
//! zero-copy fast path) and [`codex::CodexProvider`] (Messages↔Responses
//! translation with a live SSE transform — it bypasses the whole-body trait
//! hooks, which cannot express streaming transforms; see
//! `proxy::sse::SseTransform`). Compile-checked design stubs remain in
//! [`stubs`].

pub mod anthropic;
pub mod codex;
pub mod stubs;

use crate::config::AccountCredential;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// The provider is a v0.1 design stub — wired into the type system but
    /// intentionally not functional (spec §Non-goals).
    #[error("provider {provider} is a design stub, not implemented in v0.1")]
    NotImplemented { provider: &'static str },
    #[error("credential injection failed: {0}")]
    Auth(String),
    #[error("format conversion failed: {0}")]
    Convert(String),
}

/// Anthropic-shaped request as received from the client (Claude Code).
#[derive(Debug, Clone)]
pub struct AnthropicRequest {
    pub method: http::Method,
    /// Path + query relative to the base URL (e.g. `/v1/messages`).
    pub path: String,
    pub headers: http::HeaderMap,
    pub body: bytes::Bytes,
}

/// Anthropic-shaped response as delivered back to the client.
#[derive(Debug, Clone)]
pub struct AnthropicResponse {
    pub status: http::StatusCode,
    pub headers: http::HeaderMap,
    pub body: bytes::Bytes,
}

/// Request in the target provider's native wire format, ready to send to
/// [`Provider::endpoint`].
#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub method: http::Method,
    pub path: String,
    pub headers: http::HeaderMap,
    pub body: bytes::Bytes,
}

/// Response in the provider's native wire format, as received upstream.
#[derive(Debug, Clone)]
pub struct ProviderResponse {
    pub status: http::StatusCode,
    pub headers: http::HeaderMap,
    pub body: bytes::Bytes,
}

/// Unified intermediate request (the hourglass waist). DRAFT: it wraps the
/// Anthropic wire shape so the passthrough fast path stays zero-copy; real
/// cross-provider fields are added when the first non-Anthropic backend
/// becomes more than a stub.
#[derive(Debug, Clone)]
pub struct UnifiedRequest {
    /// Model id extracted from the body, when present (future routing key).
    pub model: Option<String>,
    /// Whether the client requested SSE streaming.
    pub stream: bool,
    pub wire: AnthropicRequest,
}

/// Unified intermediate response. Same draft caveat as [`UnifiedRequest`].
#[derive(Debug, Clone)]
pub struct UnifiedResponse {
    pub wire: AnthropicResponse,
}

/// The transformer trait (FR4). Conversion hooks are fallible — identity
/// (infallible) for the Anthropic passthrough, `Err(NotImplemented)` for the
/// v0.1 design stubs; that is how stubs stay compile-checked without
/// panicking. Dyn-compatibility of `auth` is deferred to the implementation
/// stage (the proxy may hold an enum instead of `dyn Provider`).
#[allow(async_fn_in_trait)]
pub trait Provider: Send + Sync {
    /// Stable provider name (`anthropic`, `openai-codex`, `gemini`, `local`).
    fn name(&self) -> &'static str;

    /// Upstream base URL requests for this provider are sent to.
    fn endpoint(&self) -> &str;

    /// Inject the account's credential into an outgoing provider request
    /// (e.g. `Authorization: Bearer ...` or `x-api-key`).
    async fn auth(
        &self,
        req: &mut ProviderRequest,
        account: &AccountCredential,
    ) -> Result<(), ProviderError>;

    /// Anthropic-shaped client request → unified intermediate.
    fn request_out(&self, anthropic_req: AnthropicRequest)
        -> Result<UnifiedRequest, ProviderError>;

    /// Unified intermediate → provider-native request.
    fn request_in(&self, unified: UnifiedRequest) -> Result<ProviderRequest, ProviderError>;

    /// Provider-native response → unified intermediate.
    fn response_in(
        &self,
        provider_resp: ProviderResponse,
    ) -> Result<UnifiedResponse, ProviderError>;

    /// Unified intermediate → Anthropic-shaped client response.
    fn response_out(&self, unified: UnifiedResponse) -> Result<AnthropicResponse, ProviderError>;
}
