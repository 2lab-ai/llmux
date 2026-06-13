//! PKCE OAuth flow against claude.ai: browser authorize, code exchange,
//! token refresh. Constants verified against teamclaude source
//! (see `.prd/02-architecture.md` §OAuth constants).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncBufReadExt;
use tokio::sync::{oneshot, Mutex};

use super::AuthError;

/// OAuth client id used by Claude Code / teamclaude.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Browser authorization endpoint.
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";

/// Token exchange + refresh endpoint.
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";

/// Scope string, verbatim from teamclaude `src/oauth.js` (`OAUTH_SCOPES`).
pub const SCOPES: &str = "org:create_api_key user:profile user:inference \
                          user:sessions:claude_code user:mcp_servers user:file_upload";

/// Where the browser tab lands after a successful callback.
const SUCCESS_REDIRECT: &str = "https://platform.claude.com/oauth/code/success?app=claude-code";

/// Interactive login gives up after this long (teamclaude: 2 minutes).
const LOGIN_TIMEOUT: Duration = Duration::from_secs(120);

/// Refresh retry policy: up to 2 retries (3 attempts) on 5xx / network
/// errors, backing off through this ladder.
const REFRESH_MAX_RETRIES: usize = 2;
const REFRESH_RETRY_DELAYS_MS: [u64; 3] = [500, 1000, 2000];

/// A completed refresh outcome is served to coalesced callers arriving
/// within this window (covers callers that raced the in-flight refresh).
const COALESCE_REUSE_WINDOW: Duration = Duration::from_secs(60);

/// PKCE verifier + S256 challenge pair for one login attempt.
#[derive(Debug, Clone)]
pub struct PkcePair {
    /// High-entropy random verifier, sent on code exchange.
    pub verifier: String,
    /// `base64url(sha256(verifier))`, sent on authorize.
    pub challenge: String,
}

impl PkcePair {
    /// Generate a fresh verifier and its S256 challenge.
    pub fn generate() -> Self {
        Self::from_verifier(random_b64url_32())
    }

    /// Derive the S256 challenge for a known verifier (test seam; `generate`
    /// is the production entry point).
    fn from_verifier(verifier: String) -> Self {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

/// 32 CSPRNG bytes, base64url without padding (43 chars).
fn random_b64url_32() -> String {
    let mut bytes = [0u8; 32];
    if let Err(err) = getrandom::fill(&mut bytes) {
        // A failing OS CSPRNG must never degrade to weaker entropy for an
        // OAuth verifier/state — abort instead.
        panic!("OS CSPRNG unavailable: {err}");
    }
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Tokens as returned by the token endpoint, normalized.
#[derive(Debug, Clone)]
pub struct OAuthTokens {
    pub access_token: String,
    /// Refresh may omit a new refresh token — `None` means "keep the old one"
    /// (callers must preserve on absence).
    pub refresh_token: Option<String>,
    /// Expiry in epoch milliseconds, already normalized via
    /// [`normalize_expires_at_ms`].
    pub expires_at_ms: u64,
}

/// Build the browser authorize URL (PKCE S256, `state` echo check,
/// localhost callback `redirect_uri`).
pub fn authorize_url(pkce: &PkcePair, state: &str, redirect_uri: &str) -> String {
    let params: [(&str, &str); 8] = [
        ("code", "true"),
        ("client_id", CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPES),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ];
    let mut url = String::from(AUTHORIZE_URL);
    for (i, (key, value)) in params.iter().enumerate() {
        url.push(if i == 0 { '?' } else { '&' });
        url.push_str(key);
        url.push('=');
        url.push_str(&urlencode(value));
    }
    url
}

/// Percent-encode everything outside the RFC 3986 unreserved set.
pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                // Writing to a String cannot fail.
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Full interactive login: bind a localhost callback on port 0, open the
/// browser, and race the callback against a manual-paste fallback on stdin.
/// Verifies `state` before exchanging the code.
pub async fn login_interactive(client: &reqwest::Client) -> Result<OAuthTokens, AuthError> {
    let pkce = PkcePair::generate();
    let state = random_b64url_32();

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{port}/callback");

    let (code_tx, code_rx) = oneshot::channel::<Result<String, AuthError>>();
    let app = Router::new()
        .route("/callback", get(callback_handler))
        .with_state(CallbackState {
            expected_state: state.clone(),
            code_tx: Arc::new(StdMutex::new(Some(code_tx))),
        });
    let server = tokio::spawn(async move {
        // The task is aborted once a code is obtained; serve errors at that
        // point are expected and irrelevant.
        let _ = axum::serve(listener, app).await;
    });

    let url = authorize_url(&pkce, &state, &redirect_uri);
    eprintln!("Opening browser for authentication...");
    eprintln!("If it doesn't open, visit:\n  {url}\n");
    open_browser(&url);

    let code = wait_for_code(code_rx, &state).await;
    server.abort();
    let code = code?;

    exchange_code_with_state(client, TOKEN_URL, &code, Some(&state), &pkce, &redirect_uri).await
}

/// Race the callback server against manual paste on stdin (TTY only,
/// mirroring teamclaude) under the 2-minute login timeout.
async fn wait_for_code(
    code_rx: oneshot::Receiver<Result<String, AuthError>>,
    expected_state: &str,
) -> Result<String, AuthError> {
    let timeout = tokio::time::sleep(LOGIN_TIMEOUT);
    tokio::pin!(timeout);
    let mut code_rx = code_rx;

    let mut stdin_open = std::io::stdin().is_terminal();
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    if stdin_open {
        eprintln!("Paste authorization code here (or wait for browser callback):");
    }

    loop {
        tokio::select! {
            received = &mut code_rx => {
                return received.unwrap_or_else(|_| {
                    Err(AuthError::Aborted("callback server closed unexpectedly".into()))
                });
            }
            () = &mut timeout => {
                return Err(AuthError::Aborted("login timed out after 2 minutes".into()));
            }
            line = lines.next_line(), if stdin_open => {
                match line {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue; // keep waiting for the callback
                        }
                        return parse_pasted_code(trimmed, expected_state);
                    }
                    // stdin closed or unreadable: fall back to callback-only.
                    Ok(None) | Err(_) => stdin_open = false,
                }
            }
        }
    }
}

/// Interpret a manually pasted value: a full callback URL (extract `code`,
/// verify `state` when present) or the bare authorization code.
/// Query values are not percent-decoded — codes and state are base64url.
fn parse_pasted_code(input: &str, expected_state: &str) -> Result<String, AuthError> {
    if input.starts_with("http://") || input.starts_with("https://") {
        if let Some((_, query)) = input.split_once('?') {
            let query = query.split('#').next().unwrap_or(query);
            let params = parse_query(query);
            if let Some(code) = params.get("code") {
                if let Some(state) = params.get("state") {
                    if state != expected_state {
                        return Err(AuthError::StateMismatch);
                    }
                }
                return Ok(code.clone());
            }
        }
    }
    // Anything else is treated as the raw authorization code (teamclaude
    // semantics).
    Ok(input.to_string())
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// One-shot slot the callback handler fires the login outcome through;
/// `take()`n on first settle so later hits are ignored.
type CodeSender = Arc<StdMutex<Option<oneshot::Sender<Result<String, AuthError>>>>>;

#[derive(Clone)]
struct CallbackState {
    expected_state: String,
    code_tx: CodeSender,
}

/// What one `/callback` hit means for the login flow. Pure — unit-tested
/// without sockets.
#[derive(Debug, PartialEq, Eq)]
enum CallbackOutcome {
    /// Valid code with matching state.
    Code(String),
    /// Provider sent `error=...` — login fails.
    ProviderError(String),
    /// `state` missing or different — reject (possible CSRF).
    StateMismatch,
    /// No code and no error (stray hit) — 404, keep waiting.
    Ignore,
}

fn eval_callback(params: &HashMap<String, String>, expected_state: &str) -> CallbackOutcome {
    if let Some(error) = params.get("error") {
        let description = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or("");
        return CallbackOutcome::ProviderError(format!("{error} - {description}"));
    }
    match params.get("state") {
        Some(state) if state == expected_state => {}
        _ => return CallbackOutcome::StateMismatch,
    }
    match params.get("code") {
        Some(code) if !code.is_empty() => CallbackOutcome::Code(code.clone()),
        _ => CallbackOutcome::Ignore,
    }
}

async fn callback_handler(
    State(state): State<CallbackState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let outcome = eval_callback(&params, &state.expected_state);

    let settle = |result: Result<String, AuthError>| {
        let mut slot = state
            .code_tx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(tx) = slot.take() {
            let _ = tx.send(result);
        }
    };

    match outcome {
        CallbackOutcome::Code(code) => {
            settle(Ok(code));
            (StatusCode::FOUND, [(header::LOCATION, SUCCESS_REDIRECT)]).into_response()
        }
        CallbackOutcome::ProviderError(message) => {
            settle(Err(AuthError::Aborted(format!("oauth error: {message}"))));
            failure_page()
        }
        CallbackOutcome::StateMismatch => {
            settle(Err(AuthError::StateMismatch));
            failure_page()
        }
        CallbackOutcome::Ignore => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

fn failure_page() -> Response {
    Html("<html><body><h2>Authentication failed</h2><p>You can close this tab.</p></body></html>")
        .into_response()
}

/// Launch the platform browser opener, best-effort (the URL is also printed
/// for manual use, so failures are ignored).
fn open_browser(url: &str) {
    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start", "", url])
    } else {
        ("xdg-open", vec![url])
    };
    let _ = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Exchange an authorization code for tokens (PKCE verifier included).
pub async fn exchange_code(
    client: &reqwest::Client,
    code: &str,
    pkce: &PkcePair,
    redirect_uri: &str,
) -> Result<OAuthTokens, AuthError> {
    exchange_code_with_state(client, TOKEN_URL, code, None, pkce, redirect_uri).await
}

/// Code exchange with an injectable endpoint (tests) and optional `state`
/// echo (teamclaude includes it; `login_interactive` always has one).
async fn exchange_code_with_state(
    client: &reqwest::Client,
    token_url: &str,
    code: &str,
    state: Option<&str>,
    pkce: &PkcePair,
    redirect_uri: &str,
) -> Result<OAuthTokens, AuthError> {
    let mut body = serde_json::json!({
        "code": code,
        "grant_type": "authorization_code",
        "client_id": CLIENT_ID,
        "redirect_uri": redirect_uri,
        "code_verifier": pkce.verifier,
    });
    if let (Some(state), Some(map)) = (state, body.as_object_mut()) {
        map.insert("state".into(), serde_json::Value::String(state.to_string()));
    }

    let response = client
        .post(token_url)
        .header(header::ACCEPT, "application/json, text/plain, */*")
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::TokenEndpoint { status, body });
    }
    let text = response.text().await?;
    let parsed: TokenResponse = serde_json::from_str(&text)?;
    Ok(parsed.into_tokens())
}

/// One token refresh against [`TOKEN_URL`]. Coalescing is the caller's job —
/// see [`RefreshCoalescer`].
pub async fn refresh(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<OAuthTokens, AuthError> {
    refresh_at(client, TOKEN_URL, refresh_token).await
}

/// Refresh with an injectable endpoint (tests hit a local mock).
///
/// Retry taxonomy (teamclaude semantics):
/// - 5xx / network errors: retry through [`REFRESH_RETRY_DELAYS_MS`],
///   at most [`REFRESH_MAX_RETRIES`] retries.
/// - 401 or an `invalid_grant` body: [`AuthError::RefreshPermanent`] —
///   no retry, re-login required.
/// - other non-2xx: [`AuthError::TokenEndpoint`], no retry.
pub async fn refresh_at(
    client: &reqwest::Client,
    token_url: &str,
    refresh_token: &str,
) -> Result<OAuthTokens, AuthError> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLIENT_ID,
    });

    let mut attempt = 0usize;
    loop {
        if attempt > 0 {
            let index = (attempt - 1).min(REFRESH_RETRY_DELAYS_MS.len() - 1);
            tokio::time::sleep(Duration::from_millis(REFRESH_RETRY_DELAYS_MS[index])).await;
        }

        let response = match client
            .post(token_url)
            .header(header::ACCEPT, "application/json, text/plain, */*")
            .json(&body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                if attempt < REFRESH_MAX_RETRIES {
                    attempt += 1;
                    continue;
                }
                return Err(AuthError::Network(err.to_string()));
            }
        };

        let status = response.status();
        if status.is_success() {
            let text = response
                .text()
                .await
                .map_err(|err| AuthError::Network(err.to_string()))?;
            let parsed: TokenResponse = serde_json::from_str(&text)?;
            return Ok(parsed.into_tokens());
        }

        let body_text = response.text().await.unwrap_or_default();
        if status == StatusCode::UNAUTHORIZED || body_text.contains("invalid_grant") {
            return Err(AuthError::RefreshPermanent {
                status,
                body: body_text,
            });
        }
        if status.is_server_error() && attempt < REFRESH_MAX_RETRIES {
            attempt += 1;
            continue;
        }
        return Err(AuthError::TokenEndpoint {
            status,
            body: body_text,
        });
    }
}

/// Wire shape of the token endpoint response (exchange and refresh).
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<u64>,
    #[serde(default)]
    expires_in: Option<u64>,
}

impl TokenResponse {
    fn into_tokens(self) -> OAuthTokens {
        let expires_at_ms = self
            .expires_at
            .filter(|&at| at > 0)
            .map(normalize_expires_at_ms)
            .unwrap_or_else(|| now_ms() + self.expires_in.unwrap_or(3600) * 1000);
        OAuthTokens {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            expires_at_ms,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Normalize an `expires_at` that may arrive in seconds OR milliseconds:
/// values `< 1e12` are seconds and get multiplied by 1000.
pub fn normalize_expires_at_ms(raw: u64) -> u64 {
    if raw < 1_000_000_000_000 {
        raw * 1000
    } else {
        raw
    }
}

/// Per-account refresh coalescing (`.prd/02-architecture.md` §Concurrency
/// model): while one refresh is in flight for an account, concurrent callers
/// await the same outcome instead of firing their own. Persisting refreshed
/// tokens back to disk (`config::update`, read-merge-write) is the caller's
/// composition — this type only owns the in-flight dedup.
#[derive(Debug)]
pub struct RefreshCoalescer {
    token_url: String,
    slots: StdMutex<HashMap<String, Arc<Mutex<Slot>>>>,
}

/// Last completed refresh for one account, keyed by the refresh token it
/// consumed. Callers that were queued behind the in-flight refresh (same
/// token, within [`COALESCE_REUSE_WINDOW`]) get this outcome instead of
/// firing another HTTP request.
#[derive(Debug, Default)]
struct Slot {
    last: Option<(String, Instant, Result<OAuthTokens, AuthError>)>,
}

impl Default for RefreshCoalescer {
    fn default() -> Self {
        Self {
            token_url: TOKEN_URL.to_string(),
            slots: StdMutex::new(HashMap::new()),
        }
    }
}

impl RefreshCoalescer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Coalescer pointed at a non-default token endpoint (tests).
    pub fn with_token_url(token_url: impl Into<String>) -> Self {
        Self {
            token_url: token_url.into(),
            slots: StdMutex::new(HashMap::new()),
        }
    }

    /// Refresh tokens for `account_name`, coalescing with any in-flight
    /// refresh for the same account.
    pub async fn refresh(
        &self,
        client: &reqwest::Client,
        account_name: &str,
        refresh_token: &str,
    ) -> Result<OAuthTokens, AuthError> {
        let slot = {
            let mut slots = self
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(slots.entry(account_name.to_string()).or_default())
        };

        // Followers park here while the leader holds the lock and performs
        // the actual refresh.
        let mut guard = slot.lock().await;
        if let Some((token, completed_at, outcome)) = guard.last.as_ref() {
            if token == refresh_token && completed_at.elapsed() < COALESCE_REUSE_WINDOW {
                return clone_outcome(outcome);
            }
        }

        let outcome = refresh_at(client, &self.token_url, refresh_token).await;
        guard.last = Some((
            refresh_token.to_string(),
            Instant::now(),
            clone_outcome(&outcome),
        ));
        outcome
    }
}

/// `AuthError` is not `Clone` (reqwest/serde/io sources aren't); coalesced
/// waiters get a structurally equal copy for the variants `refresh_at` can
/// produce, and a stringified `Aborted` for anything else.
fn clone_outcome(outcome: &Result<OAuthTokens, AuthError>) -> Result<OAuthTokens, AuthError> {
    match outcome {
        Ok(tokens) => Ok(tokens.clone()),
        Err(AuthError::TokenEndpoint { status, body }) => Err(AuthError::TokenEndpoint {
            status: *status,
            body: body.clone(),
        }),
        Err(AuthError::RefreshPermanent { status, body }) => Err(AuthError::RefreshPermanent {
            status: *status,
            body: body.clone(),
        }),
        Err(AuthError::Network(message)) => Err(AuthError::Network(message.clone())),
        Err(AuthError::StateMismatch) => Err(AuthError::StateMismatch),
        Err(AuthError::Aborted(message)) => Err(AuthError::Aborted(message.clone())),
        Err(other) => Err(AuthError::Aborted(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::routing::post;

    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        // Appendix B of RFC 7636.
        let pair =
            PkcePair::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_string());
        assert_eq!(
            pair.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn pkce_generate_shape() {
        let a = PkcePair::generate();
        let b = PkcePair::generate();
        // 32 bytes base64url unpadded = 43 chars; sha256 digest likewise.
        assert_eq!(a.verifier.len(), 43);
        assert_eq!(a.challenge.len(), 43);
        assert!(a
            .verifier
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
        assert_ne!(a.verifier, b.verifier, "verifiers must be random");
        // Challenge is deterministic over the verifier.
        assert_eq!(
            a.challenge,
            PkcePair::from_verifier(a.verifier.clone()).challenge
        );
    }

    #[test]
    fn normalize_expires_at_boundaries() {
        assert_eq!(normalize_expires_at_ms(1_700_000_000), 1_700_000_000_000); // seconds
        assert_eq!(
            normalize_expires_at_ms(1_700_000_000_000),
            1_700_000_000_000
        ); // already ms
        assert_eq!(
            normalize_expires_at_ms(999_999_999_999),
            999_999_999_999_000
        ); // < 1e12 → s
        assert_eq!(
            normalize_expires_at_ms(1_000_000_000_000),
            1_000_000_000_000
        ); // == 1e12 → ms
        assert_eq!(normalize_expires_at_ms(0), 0);
    }

    #[test]
    fn authorize_url_contains_required_params() {
        let pkce = PkcePair::from_verifier("test-verifier".to_string());
        let url = authorize_url(&pkce, "the-state", "http://localhost:7777/callback");
        assert!(url.starts_with("https://claude.ai/oauth/authorize?code=true&"));
        assert!(url.contains("client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A7777%2Fcallback"));
        assert!(url.contains("scope=org%3Acreate_api_key%20user%3Aprofile%20user%3Ainference"));
        assert!(url.contains(&format!("code_challenge={}", pkce.challenge)));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=the-state"));
    }

    #[test]
    fn callback_state_mismatch_rejected() {
        let params: HashMap<String, String> = [
            ("code".to_string(), "abc".to_string()),
            ("state".to_string(), "evil".to_string()),
        ]
        .into();
        assert_eq!(
            eval_callback(&params, "good"),
            CallbackOutcome::StateMismatch
        );

        // Missing state is also a mismatch.
        let params: HashMap<String, String> = [("code".to_string(), "abc".to_string())].into();
        assert_eq!(
            eval_callback(&params, "good"),
            CallbackOutcome::StateMismatch
        );
    }

    #[test]
    fn callback_happy_error_and_stray_paths() {
        let params: HashMap<String, String> = [
            ("code".to_string(), "abc".to_string()),
            ("state".to_string(), "good".to_string()),
        ]
        .into();
        assert_eq!(
            eval_callback(&params, "good"),
            CallbackOutcome::Code("abc".to_string())
        );

        let params: HashMap<String, String> = [
            ("error".to_string(), "access_denied".to_string()),
            ("error_description".to_string(), "nope".to_string()),
        ]
        .into();
        assert_eq!(
            eval_callback(&params, "good"),
            CallbackOutcome::ProviderError("access_denied - nope".to_string())
        );

        // No code, no error, valid state → stray hit, keep waiting.
        let params: HashMap<String, String> = [("state".to_string(), "good".to_string())].into();
        assert_eq!(eval_callback(&params, "good"), CallbackOutcome::Ignore);
    }

    #[test]
    fn pasted_code_accepts_url_and_raw_forms() {
        // Full callback URL with matching state.
        let url = "http://localhost:1234/callback?code=the-code&state=st";
        assert_eq!(parse_pasted_code(url, "st").ok(), Some("the-code".into()));

        // URL with wrong state is rejected.
        let url = "http://localhost:1234/callback?code=the-code&state=evil";
        assert!(matches!(
            parse_pasted_code(url, "st"),
            Err(AuthError::StateMismatch)
        ));

        // Raw code passes through.
        assert_eq!(
            parse_pasted_code("raw-code-123", "st").ok(),
            Some("raw-code-123".into())
        );

        // Fragment after the query is ignored.
        let url = "https://x/callback?code=c1&state=st#frag";
        assert_eq!(parse_pasted_code(url, "st").ok(), Some("c1".into()));
    }

    /// Mock token endpoint: serves `responses` in order (last one repeats),
    /// counting attempts.
    async fn spawn_token_mock(
        responses: Vec<(StatusCode, String)>,
        delay: Duration,
    ) -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_in_handler = Arc::clone(&hits);
        let responses = Arc::new(responses);
        let app = Router::new().route(
            "/v1/oauth/token",
            post(move || {
                let hits = Arc::clone(&hits_in_handler);
                let responses = Arc::clone(&responses);
                async move {
                    let i = hits.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(delay).await;
                    let (status, body) = responses[i.min(responses.len() - 1)].clone();
                    (status, body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1/oauth/token"), hits)
    }

    fn ok_token_body(expires_at: u64) -> String {
        format!(r#"{{"access_token":"at-new","refresh_token":"rt-new","expires_at":{expires_at}}}"#)
    }

    #[tokio::test]
    async fn refresh_retries_5xx_then_succeeds() {
        let (url, hits) = spawn_token_mock(
            vec![
                (StatusCode::INTERNAL_SERVER_ERROR, "boom".into()),
                (StatusCode::BAD_GATEWAY, "boom".into()),
                (StatusCode::OK, ok_token_body(1_700_000_000)), // seconds on purpose
            ],
            Duration::ZERO,
        )
        .await;
        let client = reqwest::Client::new();
        let tokens = refresh_at(&client, &url, "rt-old").await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        assert_eq!(tokens.access_token, "at-new");
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt-new"));
        assert_eq!(tokens.expires_at_ms, 1_700_000_000_000); // normalized to ms
    }

    #[tokio::test]
    async fn refresh_5xx_exhausts_retries() {
        let (url, hits) = spawn_token_mock(
            vec![(StatusCode::INTERNAL_SERVER_ERROR, "down".into())],
            Duration::ZERO,
        )
        .await;
        let client = reqwest::Client::new();
        let err = refresh_at(&client, &url, "rt-old").await.unwrap_err();
        assert_eq!(hits.load(Ordering::SeqCst), 1 + REFRESH_MAX_RETRIES);
        assert!(matches!(err, AuthError::TokenEndpoint { status, .. }
            if status == StatusCode::INTERNAL_SERVER_ERROR));
    }

    #[tokio::test]
    async fn refresh_401_is_permanent_no_retry() {
        let (url, hits) = spawn_token_mock(
            vec![(StatusCode::UNAUTHORIZED, "no".into())],
            Duration::ZERO,
        )
        .await;
        let client = reqwest::Client::new();
        let err = refresh_at(&client, &url, "rt-old").await.unwrap_err();
        assert_eq!(hits.load(Ordering::SeqCst), 1, "401 must not be retried");
        assert!(matches!(err, AuthError::RefreshPermanent { status, .. }
            if status == StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn refresh_invalid_grant_is_permanent_no_retry() {
        let (url, hits) = spawn_token_mock(
            vec![(
                StatusCode::BAD_REQUEST,
                r#"{"error":"invalid_grant"}"#.into(),
            )],
            Duration::ZERO,
        )
        .await;
        let client = reqwest::Client::new();
        let err = refresh_at(&client, &url, "rt-old").await.unwrap_err();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(matches!(err, AuthError::RefreshPermanent { .. }));
    }

    #[tokio::test]
    async fn refresh_network_error_exhausts_retries() {
        // Nothing is listening on this port (bind then drop to reserve-and-free).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/oauth/token", listener.local_addr().unwrap());
        drop(listener);
        let client = reqwest::Client::new();
        let err = refresh_at(&client, &url, "rt-old").await.unwrap_err();
        assert!(matches!(err, AuthError::Network(_)));
    }

    #[tokio::test]
    async fn refresh_response_may_omit_refresh_token() {
        let (url, _) = spawn_token_mock(
            vec![(
                StatusCode::OK,
                r#"{"access_token":"at2","expires_in":600}"#.into(),
            )],
            Duration::ZERO,
        )
        .await;
        let client = reqwest::Client::new();
        let tokens = refresh_at(&client, &url, "rt-old").await.unwrap();
        // None = "keep the old one"; the caller preserves it.
        assert_eq!(tokens.refresh_token, None);
        assert!(tokens.expires_at_ms > now_ms());
    }

    #[tokio::test]
    async fn coalescer_dedupes_concurrent_refreshes() {
        let (url, hits) = spawn_token_mock(
            vec![(StatusCode::OK, ok_token_body(9_999_999_999_999))],
            Duration::from_millis(200),
        )
        .await;
        let client = reqwest::Client::new();
        let coalescer = RefreshCoalescer::with_token_url(&url);

        let (a, b) = tokio::join!(
            coalescer.refresh(&client, "acct", "rt-1"),
            coalescer.refresh(&client, "acct", "rt-1"),
        );
        assert_eq!(a.unwrap().access_token, "at-new");
        assert_eq!(b.unwrap().access_token, "at-new");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "concurrent refreshes for one account must coalesce into one request"
        );
    }

    #[tokio::test]
    async fn coalescer_separate_accounts_do_not_coalesce() {
        let (url, hits) = spawn_token_mock(
            vec![(StatusCode::OK, ok_token_body(9_999_999_999_999))],
            Duration::from_millis(50),
        )
        .await;
        let client = reqwest::Client::new();
        let coalescer = RefreshCoalescer::with_token_url(&url);

        let (a, b) = tokio::join!(
            coalescer.refresh(&client, "acct-a", "rt-a"),
            coalescer.refresh(&client, "acct-b", "rt-b"),
        );
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn coalescer_replays_permanent_failure_to_waiters() {
        let (url, hits) = spawn_token_mock(
            vec![(StatusCode::UNAUTHORIZED, "dead token".into())],
            Duration::from_millis(100),
        )
        .await;
        let client = reqwest::Client::new();
        let coalescer = RefreshCoalescer::with_token_url(&url);

        let (a, b) = tokio::join!(
            coalescer.refresh(&client, "acct", "rt-dead"),
            coalescer.refresh(&client, "acct", "rt-dead"),
        );
        assert!(matches!(a.unwrap_err(), AuthError::RefreshPermanent { .. }));
        assert!(matches!(b.unwrap_err(), AuthError::RefreshPermanent { .. }));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exchange_code_posts_verifier_and_normalizes() {
        let (url, hits) = spawn_token_mock(
            vec![(StatusCode::OK, ok_token_body(1_800_000_000))],
            Duration::ZERO,
        )
        .await;
        let client = reqwest::Client::new();
        let pkce = PkcePair::from_verifier("v".repeat(43));
        let tokens = exchange_code_with_state(
            &client,
            &url,
            "the-code",
            Some("st"),
            &pkce,
            "http://localhost:1/callback",
        )
        .await
        .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(tokens.expires_at_ms, 1_800_000_000_000);
    }
}
