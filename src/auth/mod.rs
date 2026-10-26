//! Account authentication: PKCE OAuth login, token refresh (coalesced),
//! profile lookup, and `~/.claude/.credentials.json` import.

pub mod claude_resets;
pub mod codex;
pub mod codex_usage;
pub mod credentials;
pub mod gemini;
pub mod gemini_usage;
pub mod grok;
pub mod grok_usage;
pub mod oauth;
pub mod openrouter;
pub mod plan;
pub mod profile;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("oauth http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("token endpoint returned {status}: {body}")]
    TokenEndpoint {
        status: http::StatusCode,
        body: String,
    },
    #[error("auth response parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("oauth state mismatch (possible CSRF, aborting)")]
    StateMismatch,
    #[error("login flow aborted: {0}")]
    Aborted(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Refresh failed in a way a retry cannot fix (401 / `invalid_grant`):
    /// the refresh token is dead and the account needs a fresh login.
    #[error("token refresh permanently failed ({status}): {body}")]
    RefreshPermanent {
        status: http::StatusCode,
        body: String,
    },
    /// Transport-level failure (connect/reset/timeout) after retries.
    #[error("network error: {0}")]
    Network(String),
    #[error("profile endpoint returned {status}: {body}")]
    ProfileEndpoint {
        status: http::StatusCode,
        body: String,
    },
    /// Profile response was 200 but lacked a field llmux cannot work
    /// without (`account.uuid` is the dedup key; `account.email` the name).
    #[error("profile response missing {0}")]
    ProfileIncomplete(&'static str),
    /// `~/.codex/auth.json` (or the codex token response) lacked a field
    /// the codex provider cannot work without.
    #[error("codex auth data invalid: {0}")]
    CodexAuth(&'static str),
    /// An xAI discovery/device/token response lacked or malformed a field
    /// the grok flow cannot work without.
    #[error("grok auth data invalid: {0}")]
    GrokAuth(&'static str),
    /// A Google token/userinfo/loadCodeAssist response or the Antigravity
    /// CLI token store lacked or malformed a field the gemini flow cannot
    /// work without.
    #[error("gemini auth data invalid: {0}")]
    GeminiAuth(&'static str),
    /// No Google OAuth client for the gemini login / token refresh: llmux
    /// ships none, the operator configures it
    /// (`crate::config::GeminiConfig::oauth_client`). A CONFIGURATION
    /// failure — never a verdict on any account's refresh token.
    /// The daemon resolves the client once at startup, hence the restart
    /// hint: keys added while it runs take effect after a restart.
    #[error(
        "gemini OAuth client not configured: set gemini.client_id and gemini.client_secret \
         in llmux.json (or LLMUX_GEMINI_CLIENT_ID / LLMUX_GEMINI_CLIENT_SECRET) \
         — the daemon reads them at startup, so restart llmux after setting them"
    )]
    GeminiClientNotConfigured,
}

impl AuthError {
    /// HTTP evidence only; transport/parse failures must not invent a status.
    pub fn http_status(&self) -> Option<http::StatusCode> {
        match self {
            Self::Http(err) => err.status(),
            Self::TokenEndpoint { status, .. }
            | Self::RefreshPermanent { status, .. }
            | Self::ProfileEndpoint { status, .. } => Some(*status),
            _ => None,
        }
    }
}
