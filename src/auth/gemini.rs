//! Google Antigravity subscription auth (docs/gemini/spec.md §R2): the
//! installed-app OAuth login against accounts.google.com (loopback callback
//! on 51121, the operator-configured client id + secret
//! ([`GeminiOAuthClient`]), NO PKCE), refresh-token
//! grants, the userinfo email, Cloud Code project resolution
//! (`loadCodeAssist`, `onboardUser` fallback) and the offline import of the
//! Antigravity CLI token store. Ported from CLIProxyAPI @5fc580b1
//! `internal/auth/antigravity/{constants.go,auth.go}` (constants verified
//! there; see the spec's evidence table).
//!
//! Nothing here logs or formats a token: errors carry an HTTP status plus the
//! capped upstream error body, or a fixed [`AuthError::GeminiAuth`] phrase.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use http::StatusCode;
use serde_json::{json, Value};

use super::codex::jwt_payload;
use super::oauth::{self, urlencode, OAuthTokens};
use super::AuthError;
use crate::config::{AccountConfig, AccountCredential};
use crate::scheduler::headers::parse_rfc3339;

/// Antigravity's installed-app Google OAuth client — the id + secret every
/// authorize URL, code exchange and refresh grant carries. llmux ships NO
/// value: the operator configures it (`gemini.client_id` /
/// `gemini.client_secret`, or the `LLMUX_GEMINI_CLIENT_*` environment), and
/// [`crate::config::GeminiConfig::oauth_client`] is the one resolver.
#[derive(Clone, PartialEq, Eq)]
pub struct GeminiOAuthClient {
    pub id: String,
    pub secret: String,
}

/// The secret never reaches a `{:?}` (a log line, a panic message).
impl std::fmt::Debug for GeminiOAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiOAuthClient")
            .field("id", &self.id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Browser authorization endpoint (`constants.go:22`).
pub const GEMINI_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// Token endpoint for code exchange and refresh (`constants.go:21`); the
/// configured `gemini.token_url` defaults to this.
pub const GEMINI_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Userinfo endpoint that names the account (`constants.go:23`).
pub const GEMINI_USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo?alt=json";

/// The five Antigravity scopes, space-joined (`constants.go:12-18`).
pub const GEMINI_SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform \
https://www.googleapis.com/auth/userinfo.email \
https://www.googleapis.com/auth/userinfo.profile \
https://www.googleapis.com/auth/cclog \
https://www.googleapis.com/auth/experimentsandconfigs";

/// Loopback callback port registered for the client (`constants.go:8`).
/// Only this port: Google's redirect is registered for it exactly, so a bind
/// failure surfaces (callers fall back to the agy store import).
pub const GEMINI_CALLBACK_PORT: u16 = 51121;

/// Callback path: `http://localhost:51121/oauth-callback` (`auth.go:144`).
pub const GEMINI_CALLBACK_PATH: &str = "/oauth-callback";

/// Terms-of-service note printed BEFORE the browser opens, so the operator
/// decides with the risk in view. One string for every interactive Gemini
/// login: `llmux login --gemini` and the TUI's `n` picker.
pub const GEMINI_TOS_NOTE: &str = "note: Google states that using Antigravity through third-party \
tools can lead to account suspension (see docs/gemini/spec.md).";

/// Refresh retry policy, mirroring `grok::refresh_grok_at`.
const REFRESH_MAX_RETRIES: usize = 2;
const REFRESH_RETRY_DELAYS_MS: [u64; 3] = [500, 1000, 2000];

/// `onboardUser` is a long-running operation polled until `done` (`auth.go`
/// `OnboardUser`: 5 attempts, 2 s apart, 30 s per request).
const ONBOARD_MAX_ATTEMPTS: usize = 5;
const ONBOARD_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Per-request ceiling for the identity/project control calls (userinfo,
/// `loadCodeAssist`, `onboardUser`) — the reference's onboard timeout.
const CONTROL_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// `ideType` / `ide_type` the Cloud Code control plane expects.
const IDE_TYPE: &str = "ANTIGRAVITY";

/// `ide_name` in the onboardUser metadata.
const IDE_NAME: &str = "antigravity";

/// `ide_version` when the configured user agent carries none (the hub
/// version of 2026-10-15; Cloud Code rejects newer models below 2.23.0).
const DEFAULT_IDE_VERSION: &str = "2.23.1";

/// Last-resort onboard tier when `loadCodeAssist` names neither a default
/// allowed tier nor a current tier (`defaultAntigravityTierID`).
const FALLBACK_TIER_ID: &str = "free-tier";

/// Upstream error bodies carried in an [`AuthError`] are capped (bytes, cut
/// on a char boundary): enough for Google's error JSON, never a whole HTML
/// error page in a log line.
const ERROR_BODY_CAP: usize = 512;

/// Build the browser authorize URL. Parameter order is fixed (the reference
/// encodes `url.Values`, which sorts keys); values are percent-encoded via
/// the shared `oauth::urlencode`. No PKCE — the client secret authenticates
/// the exchange.
pub fn gemini_authorize_url(oauth: &GeminiOAuthClient, state: &str, redirect_uri: &str) -> String {
    let params: [(&str, &str); 7] = [
        ("access_type", "offline"),
        ("client_id", &oauth.id),
        ("prompt", "consent"),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", GEMINI_SCOPES),
        ("state", state),
    ];
    let mut url = String::from(GEMINI_AUTHORIZE_URL);
    for (i, (key, value)) in params.iter().enumerate() {
        url.push(if i == 0 { '?' } else { '&' });
        url.push_str(key);
        url.push('=');
        url.push_str(&urlencode(value));
    }
    url
}

/// Exchange an authorization code for tokens: form-encoded `code`,
/// `client_id`, `client_secret`, `redirect_uri`, `grant_type`. A non-2xx is
/// [`AuthError::TokenEndpoint`] (a code is single-use — no retry).
pub async fn exchange_gemini_code(
    client: &reqwest::Client,
    oauth: &GeminiOAuthClient,
    token_url: &str,
    code: &str,
    redirect_uri: &str,
) -> Result<OAuthTokens, AuthError> {
    let form_body = format!(
        "code={}&client_id={}&client_secret={}&redirect_uri={}&grant_type=authorization_code",
        urlencode(code),
        urlencode(&oauth.id),
        urlencode(&oauth.secret),
        urlencode(redirect_uri),
    );
    let response = post_form(client, token_endpoint(token_url), form_body)
        .await
        .map_err(|err| AuthError::Network(err.to_string()))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| AuthError::Network(err.to_string()))?;
    if !status.is_success() {
        return Err(AuthError::TokenEndpoint {
            status,
            body: capped(text),
        });
    }
    parse_token_response(&text)
}

/// One gemini token refresh: form-encoded `grant_type=refresh_token` +
/// `refresh_token` + `client_id` + `client_secret`. Retry taxonomy follows
/// grok (5xx/network retried through the ladder) with spec §R2's permanent
/// set: 400, 401, or a body naming `invalid_grant` is
/// [`AuthError::RefreshPermanent`] (re-login required). A response without
/// `refresh_token` yields `None` — the caller keeps the stored one.
pub async fn refresh_gemini(
    client: &reqwest::Client,
    oauth: &GeminiOAuthClient,
    token_url: &str,
    refresh_token: &str,
) -> Result<OAuthTokens, AuthError> {
    let token_url = token_endpoint(token_url);
    let form_body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}&client_secret={}",
        urlencode(refresh_token),
        urlencode(&oauth.id),
        urlencode(&oauth.secret),
    );

    let mut attempt = 0usize;
    loop {
        if attempt > 0 {
            let index = (attempt - 1).min(REFRESH_RETRY_DELAYS_MS.len() - 1);
            tokio::time::sleep(Duration::from_millis(REFRESH_RETRY_DELAYS_MS[index])).await;
        }

        let response = match post_form(client, token_url, form_body.clone()).await {
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
            return parse_token_response(&text);
        }

        let body_text = response.text().await.unwrap_or_default();
        if refresh_is_permanent(status, &body_text) {
            return Err(AuthError::RefreshPermanent {
                status,
                body: capped(body_text),
            });
        }
        if status.is_server_error() && attempt < REFRESH_MAX_RETRIES {
            attempt += 1;
            continue;
        }
        return Err(AuthError::TokenEndpoint {
            status,
            body: capped(body_text),
        });
    }
}

/// Spec §R2: Google answers a dead/revoked refresh token with 400
/// `invalid_grant`; 400/401 from the token endpoint cannot be fixed by a
/// retry either way.
fn refresh_is_permanent(status: StatusCode, body: &str) -> bool {
    status == StatusCode::BAD_REQUEST
        || status == StatusCode::UNAUTHORIZED
        || body.contains("invalid_grant")
}

/// POST a form body to the token endpoint.
async fn post_form(
    client: &reqwest::Client,
    token_url: &str,
    form_body: String,
) -> Result<reqwest::Response, reqwest::Error> {
    // Form-encoded by hand: reqwest's `.form()` helper sits behind a cargo
    // feature this crate does not enable.
    client
        .post(token_url)
        .header(http::header::ACCEPT, "application/json")
        .header(
            http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(form_body)
        .send()
        .await
}

/// The configured token endpoint, or Google's when the config value is blank
/// (hand-edited configs).
fn token_endpoint(token_url: &str) -> &str {
    let trimmed = token_url.trim();
    if trimmed.is_empty() {
        GEMINI_TOKEN_URL
    } else {
        trimmed
    }
}

/// Parse a Google token response. Google access tokens are opaque (not
/// JWTs), so expiry is `now + expires_in` (1h floor when absent).
fn parse_token_response(text: &str) -> Result<OAuthTokens, AuthError> {
    let value: Value = serde_json::from_str(text)?;
    let access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(AuthError::GeminiAuth("token response missing access_token"))?
        .to_string();
    let refresh_token = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let expires_in = value
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(3600);
    Ok(OAuthTokens {
        access_token,
        refresh_token,
        expires_at_ms: now_ms().saturating_add(expires_in.saturating_mul(1000)),
    })
}

/// The signed-in account's email from Google userinfo (the dedup key and
/// account name).
pub async fn fetch_email(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<String, AuthError> {
    fetch_email_at(client, GEMINI_USERINFO_URL, access_token).await
}

/// [`fetch_email`] against an injectable userinfo URL (tests / local mocks).
pub async fn fetch_email_at(
    client: &reqwest::Client,
    userinfo_url: &str,
    access_token: &str,
) -> Result<String, AuthError> {
    let response = client
        .get(userinfo_url)
        .bearer_auth(access_token)
        .header(http::header::ACCEPT, "application/json")
        .timeout(CONTROL_CALL_TIMEOUT)
        .send()
        .await
        .map_err(|err| AuthError::Network(err.to_string()))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| AuthError::Network(err.to_string()))?;
    if !status.is_success() {
        return Err(AuthError::ProfileEndpoint {
            status,
            body: capped(text),
        });
    }
    let value: Value = serde_json::from_str(&text)?;
    value
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or(AuthError::GeminiAuth("userinfo response missing email"))
}

/// Resolve the account's Cloud Code project: `POST {base}/v1internal:
/// loadCodeAssist`, and when it names no project, `onboardUser` with the
/// default tier, polled until the operation is `done`. `user_agent` is the
/// configured `gemini.user_agent` (Cloud Code gates on the hub version).
pub async fn resolve_project(
    client: &reqwest::Client,
    base: &str,
    user_agent: &str,
    access_token: &str,
) -> Result<String, AuthError> {
    resolve_project_with(
        client,
        base,
        user_agent,
        access_token,
        ONBOARD_POLL_INTERVAL,
    )
    .await
}

/// [`resolve_project`] with an injectable onboard poll interval (tests).
async fn resolve_project_with(
    client: &reqwest::Client,
    base: &str,
    user_agent: &str,
    access_token: &str,
    poll_interval: Duration,
) -> Result<String, AuthError> {
    let base = base.trim().trim_end_matches('/');
    let load = post_control(
        client,
        &format!("{base}/v1internal:loadCodeAssist"),
        user_agent,
        access_token,
        &json!({ "metadata": { "ideType": IDE_TYPE } }),
    )
    .await?;
    if let Some(project) = extract_project(&load) {
        return Ok(project);
    }

    let onboard_body = json!({
        "tier_id": onboard_tier_id(&load),
        "metadata": {
            "ide_type": IDE_TYPE,
            "ide_name": IDE_NAME,
            "ide_version": ide_version(user_agent),
        },
    });
    let onboard_url = format!("{base}/v1internal:onboardUser");
    for attempt in 0..ONBOARD_MAX_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(poll_interval).await;
        }
        let operation = post_control(
            client,
            &onboard_url,
            user_agent,
            access_token,
            &onboard_body,
        )
        .await?;
        if operation.get("done").and_then(Value::as_bool) == Some(true) {
            return operation.get("response").and_then(extract_project).ok_or(
                AuthError::GeminiAuth("onboardUser completed without a project"),
            );
        }
    }
    Err(AuthError::GeminiAuth(
        "onboardUser did not complete after 5 attempts",
    ))
}

/// One JSON control-plane POST (`loadCodeAssist` / `onboardUser`). A non-2xx
/// is [`AuthError::ProfileEndpoint`] so `http_status()` stays visible to the
/// caller (401 → refresh, 403 → ToS bench).
async fn post_control(
    client: &reqwest::Client,
    url: &str,
    user_agent: &str,
    access_token: &str,
    body: &Value,
) -> Result<Value, AuthError> {
    let response = client
        .post(url)
        .bearer_auth(access_token)
        .header(http::header::USER_AGENT, user_agent)
        .header(http::header::ACCEPT, "*/*")
        .json(body)
        .timeout(CONTROL_CALL_TIMEOUT)
        .send()
        .await
        .map_err(|err| AuthError::Network(err.to_string()))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| AuthError::Network(err.to_string()))?;
    if !status.is_success() {
        return Err(AuthError::ProfileEndpoint {
            status,
            body: capped(text),
        });
    }
    Ok(serde_json::from_str(&text)?)
}

/// First non-empty project of `cloudaicompanionProject` / `projectId` /
/// `project`, each either a string or an `{id}` object
/// (`extractCloudaicompanionProject`).
fn extract_project(value: &Value) -> Option<String> {
    ["cloudaicompanionProject", "projectId", "project"]
        .iter()
        .find_map(|key| {
            let field = value.get(*key)?;
            let id = field
                .as_str()
                .or_else(|| field.get("id").and_then(Value::as_str))?
                .trim();
            (!id.is_empty()).then(|| id.to_string())
        })
}

/// Onboard tier: the `allowedTiers` entry marked `isDefault`, else
/// `currentTier.id`, else `free-tier` (`defaultAntigravityTierID`).
fn onboard_tier_id(load: &Value) -> String {
    let non_empty_id = |tier: &Value| {
        tier.get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    };
    load.get("allowedTiers")
        .and_then(Value::as_array)
        .and_then(|tiers| {
            tiers
                .iter()
                .filter(|tier| tier.get("isDefault").and_then(Value::as_bool) == Some(true))
                .find_map(non_empty_id)
        })
        .or_else(|| load.get("currentTier").and_then(non_empty_id))
        .unwrap_or_else(|| FALLBACK_TIER_ID.to_string())
}

/// The hub version from an `antigravity/hub/<v> …` (or legacy
/// `antigravity/<v> …`) user agent, else [`DEFAULT_IDE_VERSION`]
/// (`AntigravityVersionFromUserAgent`).
fn ide_version(user_agent: &str) -> &str {
    let user_agent = user_agent.trim();
    let after = |prefix: &str| {
        user_agent
            .get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .and_then(|_| user_agent.get(prefix.len()..))
    };
    after("antigravity/hub/")
        .or_else(|| after("antigravity/"))
        .and_then(|rest| rest.split_whitespace().next())
        .filter(|version| !version.is_empty())
        .unwrap_or(DEFAULT_IDE_VERSION)
}

/// Full interactive Google login: bind the registered loopback callback
/// (51121 only), open the browser to the authorize URL, wait for the
/// state-checked callback, exchange the code, read the userinfo email and
/// resolve the Cloud Code project. Returns a ready-to-upsert
/// `AccountConfig` named `gemini:{email}`.
///
/// A project that cannot be resolved yet does NOT fail the login: an empty
/// `project_id` is the import contract ("resolved lazily on the first
/// request", `AccountCredential::Gemini`), so the minted tokens are kept and
/// the failure is shown as a warning.
///
/// `oauth` is the operator-configured client: each caller resolves it with
/// [`crate::config::GeminiConfig::oauth_client`] BEFORE calling, so an
/// unconfigured client fails before any port is bound or browser opened.
pub async fn login_gemini_interactive(
    client: &reqwest::Client,
    oauth: &GeminiOAuthClient,
    token_url: &str,
    base: &str,
    user_agent: &str,
) -> Result<AccountConfig, AuthError> {
    let state = oauth::random_b64url_32();
    let (listener, port) = oauth::bind_callback_listener(&[GEMINI_CALLBACK_PORT]).await?;
    // redirect_uri hostname is `localhost` (the client's registered value),
    // even though the listener binds 127.0.0.1.
    let redirect_uri = format!("http://localhost:{port}{GEMINI_CALLBACK_PATH}");

    let url = gemini_authorize_url(oauth, &state, &redirect_uri);
    eprintln!("Opening browser for Google (Antigravity) authentication...");
    eprintln!("If it doesn't open, visit:\n  {url}\n");
    oauth::open_browser(&url);

    let code = oauth::run_callback_server(listener, GEMINI_CALLBACK_PATH, Some(&state)).await?;
    let tokens = exchange_gemini_code(client, oauth, token_url, &code, &redirect_uri).await?;
    // Checked before any further call: without a refresh token the account
    // would die at the first access-token expiry (grok N1 contract).
    let refresh_token = login_refresh_token(&tokens)?;
    let email = fetch_email(client, &tokens.access_token).await?;
    let project_id = match resolve_project(client, base, user_agent, &tokens.access_token).await {
        Ok(project_id) => project_id,
        Err(err) => {
            eprintln!("{}", project_unresolved_warning(&err));
            String::new()
        }
    };

    Ok(gemini_account(
        email,
        tokens.access_token,
        refresh_token,
        tokens.expires_at_ms,
        project_id,
        Some(now_ms()),
    ))
}

/// The warning printed when a login or store import could not resolve the
/// Cloud Code project: the account is still saved, and the daemon resolves
/// the project on the first model request or the next usage refresh
/// (`AccountCredential::Gemini`, `proxy::gemini_project`).
pub fn project_unresolved_warning(err: &AuthError) -> String {
    format!(
        "warning: Cloud Code project not resolved yet ({err}); \
         llmux resolves it on the first request or the next usage refresh"
    )
}

/// The refresh token of an INITIAL login, required (external review N1 on
/// grok): `access_type=offline` + `prompt=consent` should always mint one.
fn login_refresh_token(tokens: &OAuthTokens) -> Result<String, AuthError> {
    tokens
        .refresh_token
        .clone()
        .filter(|token| !token.is_empty())
        .ok_or(AuthError::GeminiAuth(
            "login token response missing refresh_token (offline access not granted?)",
        ))
}

/// Account name encoding the backend group: `gemini:{email}`.
pub fn gemini_account_name(email: &str) -> String {
    format!("gemini:{email}")
}

/// Default location of the Antigravity CLI token store
/// (`$HOME/.gemini/antigravity-cli/antigravity-oauth-token`).
pub fn antigravity_store_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| {
        home.join(".gemini")
            .join("antigravity-cli")
            .join("antigravity-oauth-token")
    })
}

/// Parse the Antigravity CLI token store (offline — no network):
///
/// ```json
/// { "token": { "access_token", "token_type", "refresh_token",
///              "expiry": "<RFC 3339>" },
///   "auth_method": …, "id_token": "<JWT>" }
/// ```
///
/// Email from the `id_token`'s `email` claim, `expires_at_ms` from
/// `token.expiry` (0 = unknown when absent/unparseable — the proxy then
/// refreshes on first use), `project_id` empty (resolved on the first
/// request). A missing refresh token or email is an error: the account could
/// neither survive expiry nor be named.
pub fn account_from_antigravity_store(bytes: &[u8]) -> Result<AccountConfig, AuthError> {
    let value: Value = serde_json::from_slice(bytes)?;
    let token = value
        .get("token")
        .and_then(Value::as_object)
        .ok_or(AuthError::GeminiAuth("store missing \"token\" object"))?;
    let field = |name: &str| {
        token
            .get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let access_token = field("access_token")
        .ok_or(AuthError::GeminiAuth("store missing access_token"))?
        .to_string();
    let refresh_token = field("refresh_token")
        .ok_or(AuthError::GeminiAuth("store missing refresh_token"))?
        .to_string();
    let expires_at_ms = field("expiry")
        .and_then(parse_rfc3339)
        .map(system_time_ms)
        .unwrap_or(0);
    let email = value
        .get("id_token")
        .and_then(Value::as_str)
        .and_then(jwt_payload)
        .and_then(|claims| {
            claims
                .get("email")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .ok_or(AuthError::GeminiAuth("store id_token missing email"))?;

    Ok(gemini_account(
        email,
        access_token,
        refresh_token,
        expires_at_ms,
        String::new(),
        None,
    ))
}

fn gemini_account(
    email: String,
    access_token: String,
    refresh_token: String,
    expires_at_ms: u64,
    project_id: String,
    last_refresh_ms: Option<u64>,
) -> AccountConfig {
    AccountConfig {
        name: gemini_account_name(&email),
        credential: AccountCredential::Gemini {
            email,
            access_token,
            refresh_token,
            expires_at_ms,
            project_id,
            last_refresh_ms,
        },
    }
}

/// Cap an upstream error body at [`ERROR_BODY_CAP`] bytes on a char
/// boundary.
fn capped(mut body: String) -> String {
    if body.len() > ERROR_BODY_CAP {
        let mut cut = ERROR_BODY_CAP;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push('…');
    }
    body
}

fn system_time_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_ms() -> u64 {
    system_time_ms(SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// Unsigned JWT with the given payload (local copy of the codex test
    /// helper — that `mod tests` is private).
    fn fake_jwt(payload: &Value) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let body = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        format!("{header}.{body}.sig")
    }

    /// One request as the mock saw it.
    #[derive(Debug, Clone)]
    struct Seen {
        path: String,
        authorization: String,
        user_agent: String,
        content_type: String,
        body: String,
    }

    type Routes = HashMap<&'static str, Vec<(u16, String)>>;

    /// Local mock (never Google): per-path response queues — each hit pops
    /// the front, the last entry repeats; unknown paths answer 404. Returns
    /// (addr, every request seen in order).
    async fn mock_server(routes: Routes) -> (std::net::SocketAddr, Arc<Mutex<Vec<Seen>>>) {
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_in_handler = Arc::clone(&seen);
        let routes = Arc::new(Mutex::new(routes));
        let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let seen = Arc::clone(&seen_in_handler);
            let routes = Arc::clone(&routes);
            async move {
                let (parts, body) = req.into_parts();
                let header = |name: http::header::HeaderName| {
                    parts
                        .headers
                        .get(name)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string()
                };
                let path = parts.uri.path().to_string();
                let authorization = header(http::header::AUTHORIZATION);
                let user_agent = header(http::header::USER_AGENT);
                let content_type = header(http::header::CONTENT_TYPE);
                let bytes = axum::body::to_bytes(body, usize::MAX).await.expect("body");
                seen.lock().expect("lock").push(Seen {
                    path: path.clone(),
                    authorization,
                    user_agent,
                    content_type,
                    body: String::from_utf8_lossy(&bytes).into_owned(),
                });
                let (status, body) = {
                    let mut routes = routes.lock().expect("lock");
                    match routes.get_mut(path.as_str()) {
                        Some(queue) if queue.len() > 1 => queue.remove(0),
                        Some(queue) => queue[0].clone(),
                        None => (404, r#"{"error":"no route"}"#.to_string()),
                    }
                };
                (
                    http::StatusCode::from_u16(status).expect("status"),
                    [(http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, seen)
    }

    fn routes(entries: Vec<(&'static str, Vec<(u16, &str)>)>) -> Routes {
        entries
            .into_iter()
            .map(|(path, queue)| {
                (
                    path,
                    queue
                        .into_iter()
                        .map(|(status, body)| (status, body.to_string()))
                        .collect(),
                )
            })
            .collect()
    }

    const UA: &str = "antigravity/hub/2.23.1 darwin/arm64";

    /// Neutral client fixture: llmux ships no OAuth client, so the tests
    /// never carry one either.
    fn oauth() -> GeminiOAuthClient {
        GeminiOAuthClient {
            id: "test-client-id".into(),
            secret: "test-client-secret".into(),
        }
    }

    #[test]
    fn oauth_client_debug_never_shows_the_secret() {
        let debug = format!("{:?}", oauth());
        assert!(debug.contains("test-client-id"), "{debug}");
        assert!(!debug.contains("test-client-secret"), "{debug}");
    }

    /// llmux ships no Google OAuth client: no installed-app client id or
    /// client secret may appear anywhere in the sources or docs of this
    /// public repo. The needles are assembled at runtime so this file never
    /// matches itself.
    #[test]
    fn no_google_oauth_client_values_in_the_tree() {
        let needles = [
            ["GOC", "SPX-"].concat(),
            [".apps", ".google", "usercontent", ".com"].concat(),
        ];
        fn walk(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, files);
                } else {
                    files.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        for dir in ["src", "docs", "tests", ".github"] {
            walk(&root.join(dir), &mut files);
        }
        // Repo-root regular files (README, CHANGELOG, justfile, Cargo.toml,
        // AGENTS.md, …). Never `target/` (a directory) or `.git` (a directory,
        // or a gitlink file in a worktree).
        if let Ok(entries) = std::fs::read_dir(root) {
            files.extend(
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.is_file() && !path.ends_with(".git")),
            );
        }
        for name in ["README.md", "Cargo.toml", "justfile", "AGENTS.md"] {
            assert!(
                files.iter().any(|f| f == &root.join(name)),
                "the scan reaches the repo-root {name}"
            );
        }
        assert!(
            files.iter().any(|f| f.ends_with("src/auth/gemini.rs")),
            "the scan reaches the sources"
        );
        for file in &files {
            let Ok(text) = std::fs::read_to_string(file) else {
                continue; // binary fixtures
            };
            for needle in &needles {
                assert!(
                    !text.contains(needle.as_str()),
                    "{} carries a Google OAuth client value",
                    file.display()
                );
            }
        }
    }

    // ---- G10: authorize URL params ----
    #[test]
    fn g10_authorize_url_carries_installed_app_params_in_order() {
        let url = gemini_authorize_url(&oauth(), "st-1", "http://localhost:51121/oauth-callback");
        let expected = "https://accounts.google.com/o/oauth2/v2/auth\
             ?access_type=offline\
             &client_id=test-client-id\
             &prompt=consent\
             &redirect_uri=http%3A%2F%2Flocalhost%3A51121%2Foauth-callback\
             &response_type=code\
             &scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcloud-platform\
             %20https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.email\
             %20https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.profile\
             %20https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcclog\
             %20https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fexperimentsandconfigs\
             &state=st-1";
        assert_eq!(url, expected);
        assert!(!url.contains("code_challenge"), "no PKCE: {url}");
        assert_eq!(GEMINI_SCOPES.split(' ').count(), 5, "five scopes");
    }

    /// The ToS note is the one string both login surfaces print (CLI
    /// `--gemini`, TUI picker): pinned verbatim so a wording edit is a
    /// deliberate change to both, never a drift between them.
    #[test]
    fn tos_note_names_the_suspension_risk_and_the_spec() {
        assert_eq!(
            GEMINI_TOS_NOTE,
            "note: Google states that using Antigravity through third-party tools can lead to \
             account suspension (see docs/gemini/spec.md)."
        );
    }

    // ---- G10: code exchange form ----
    #[tokio::test]
    async fn g10_exchange_posts_code_client_secret_and_redirect() {
        let (addr, seen) = mock_server(routes(vec![(
            "/token",
            vec![(
                200,
                r#"{"access_token":"at-x","expires_in":3599,"refresh_token":"rt-x",
                    "id_token":"x.y.z","token_type":"Bearer","scope":"openid"}"#,
            )],
        )]))
        .await;
        let client = reqwest::Client::new();
        let before = now_ms();
        let tokens = exchange_gemini_code(
            &client,
            &oauth(),
            &format!("http://{addr}/token"),
            "4/0Acode",
            "http://localhost:51121/oauth-callback",
        )
        .await
        .expect("exchange");
        assert_eq!(tokens.access_token, "at-x");
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt-x"));
        assert!(
            tokens.expires_at_ms >= before + 3_599_000
                && tokens.expires_at_ms <= now_ms() + 3_599_000,
            "expires_at = now + expires_in"
        );
        let seen = seen.lock().expect("lock").clone();
        assert_eq!(seen.len(), 1);
        let form = &seen[0].body;
        assert_eq!(seen[0].content_type, "application/x-www-form-urlencoded");
        for field in [
            "code=4%2F0Acode".to_string(),
            "client_id=test-client-id".to_string(),
            "client_secret=test-client-secret".to_string(),
            "redirect_uri=http%3A%2F%2Flocalhost%3A51121%2Foauth-callback".to_string(),
            "grant_type=authorization_code".to_string(),
        ] {
            assert!(form.contains(&field), "{field} missing from {form}");
        }
        assert!(!form.contains("code_verifier"), "no PKCE: {form}");
    }

    #[tokio::test]
    async fn g10_exchange_failure_is_token_endpoint_without_retry() {
        let (addr, seen) = mock_server(routes(vec![(
            "/token",
            vec![(
                400,
                r#"{"error":"invalid_grant","error_description":"Malformed auth code."}"#,
            )],
        )]))
        .await;
        let client = reqwest::Client::new();
        let err =
            exchange_gemini_code(&client, &oauth(), &format!("http://{addr}/token"), "c", "r")
                .await
                .expect_err("rejected");
        assert!(
            matches!(err, AuthError::TokenEndpoint { status, .. } if status == StatusCode::BAD_REQUEST),
            "{err}"
        );
        assert_eq!(seen.lock().expect("lock").len(), 1, "a code is single-use");
    }

    // ---- G6: refresh form + rotation keep/replace ----
    #[tokio::test]
    async fn g6_refresh_form_carries_client_secret_and_keeps_absent_rotation_none() {
        let (addr, seen) = mock_server(routes(vec![(
            "/token",
            vec![(
                200,
                r#"{"access_token":"at-new","expires_in":3599,"token_type":"Bearer"}"#,
            )],
        )]))
        .await;
        let client = reqwest::Client::new();
        let tokens = refresh_gemini(
            &client,
            &oauth(),
            &format!("http://{addr}/token"),
            "1//rt-old",
        )
        .await
        .expect("refresh");
        assert_eq!(tokens.access_token, "at-new");
        assert!(
            tokens.refresh_token.is_none(),
            "None → the caller keeps the stored refresh token"
        );
        let seen = seen.lock().expect("lock").clone();
        let form = &seen[0].body;
        for field in [
            "grant_type=refresh_token".to_string(),
            "refresh_token=1%2F%2Frt-old".to_string(),
            "client_id=test-client-id".to_string(),
            "client_secret=test-client-secret".to_string(),
        ] {
            assert!(form.contains(&field), "{field} missing from {form}");
        }
    }

    #[tokio::test]
    async fn g6_refresh_rotation_replaces_when_the_response_carries_one() {
        let (addr, _seen) = mock_server(routes(vec![(
            "/token",
            vec![(
                200,
                r#"{"access_token":"at-new","expires_in":3599,"refresh_token":"rt-rotated"}"#,
            )],
        )]))
        .await;
        let client = reqwest::Client::new();
        let tokens = refresh_gemini(&client, &oauth(), &format!("http://{addr}/token"), "rt-old")
            .await
            .expect("refresh");
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt-rotated"));
    }

    // ---- G6: permanent set (invalid_grant / 400 / 401), never retried ----
    #[tokio::test]
    async fn g6_refresh_invalid_grant_400_and_401_are_permanent() {
        for (status, body) in [
            (
                400,
                r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
            ),
            (400, r#"{"error":"invalid_request"}"#),
            (401, r#"{"error":"unauthorized_client"}"#),
            (403, r#"{"error":"invalid_grant"}"#),
        ] {
            let (addr, seen) = mock_server(routes(vec![("/token", vec![(status, body)])])).await;
            let client = reqwest::Client::new();
            let err = refresh_gemini(
                &client,
                &oauth(),
                &format!("http://{addr}/token"),
                "rt-dead",
            )
            .await
            .expect_err("permanent");
            assert!(
                matches!(err, AuthError::RefreshPermanent { .. }),
                "{status} {body} → Permanent (re-login required): {err}"
            );
            assert_eq!(
                seen.lock().expect("lock").len(),
                1,
                "permanent is not retried"
            );
            assert!(
                !err.to_string().contains("rt-dead"),
                "no token in the error"
            );
        }
    }

    // ---- G6: 5xx retried ----
    #[tokio::test]
    async fn g6_refresh_retries_a_5xx_then_succeeds() {
        let (addr, seen) = mock_server(routes(vec![(
            "/token",
            vec![
                (503, r#"{"error":"backend_error"}"#),
                (
                    200,
                    r#"{"access_token":"at-after-retry","expires_in":3599}"#,
                ),
            ],
        )]))
        .await;
        let client = reqwest::Client::new();
        let tokens = refresh_gemini(&client, &oauth(), &format!("http://{addr}/token"), "rt")
            .await
            .expect("retried to success");
        assert_eq!(tokens.access_token, "at-after-retry");
        assert_eq!(
            seen.lock().expect("lock").len(),
            2,
            "one retry after the 503"
        );
    }

    #[tokio::test]
    async fn g6_refresh_other_4xx_is_transient_token_endpoint() {
        let (addr, seen) = mock_server(routes(vec![(
            "/token",
            vec![(429, r#"{"error":"rate_limited"}"#)],
        )]))
        .await;
        let client = reqwest::Client::new();
        let err = refresh_gemini(&client, &oauth(), &format!("http://{addr}/token"), "rt")
            .await
            .expect_err("429");
        assert!(
            matches!(err, AuthError::TokenEndpoint { status, .. } if status == StatusCode::TOO_MANY_REQUESTS),
            "{err}"
        );
        assert_eq!(seen.lock().expect("lock").len(), 1, "4xx is not retried");
    }

    // ---- S4: userinfo email ----
    #[tokio::test]
    async fn userinfo_returns_the_email_with_bearer_auth() {
        let (addr, seen) = mock_server(routes(vec![(
            "/oauth2/v2/userinfo",
            vec![(
                200,
                r#"{"id":"1","email":" a@example.com ","verified_email":true}"#,
            )],
        )]))
        .await;
        let client = reqwest::Client::new();
        let email = fetch_email_at(
            &client,
            &format!("http://{addr}/oauth2/v2/userinfo?alt=json"),
            "at-1",
        )
        .await
        .expect("email");
        assert_eq!(email, "a@example.com");
        assert_eq!(seen.lock().expect("lock")[0].authorization, "Bearer at-1");

        let (addr, _seen) = mock_server(routes(vec![(
            "/oauth2/v2/userinfo",
            vec![(200, r#"{"id":"1"}"#)],
        )]))
        .await;
        let err = fetch_email_at(
            &client,
            &format!("http://{addr}/oauth2/v2/userinfo"),
            "at-1",
        )
        .await
        .expect_err("no email");
        assert!(matches!(err, AuthError::GeminiAuth(_)), "{err}");
    }

    // ---- G8: loadCodeAssist project shapes ----
    #[tokio::test]
    async fn g8_load_code_assist_string_project() {
        let (addr, seen) = mock_server(routes(vec![(
            "/v1internal:loadCodeAssist",
            vec![(
                200,
                r#"{"currentTier":{"id":"free-tier"},"cloudaicompanionProject":"proj-str"}"#,
            )],
        )]))
        .await;
        let client = reqwest::Client::new();
        let project = resolve_project(&client, &format!("http://{addr}/"), UA, "at-1")
            .await
            .expect("project");
        assert_eq!(project, "proj-str");
        let seen = seen.lock().expect("lock").clone();
        assert_eq!(seen.len(), 1, "no onboard when the project is present");
        assert_eq!(seen[0].path, "/v1internal:loadCodeAssist");
        assert_eq!(seen[0].authorization, "Bearer at-1");
        assert_eq!(seen[0].user_agent, UA);
        assert_eq!(seen[0].content_type, "application/json");
        let body: Value = serde_json::from_str(&seen[0].body).expect("json body");
        assert_eq!(body, json!({"metadata":{"ideType":"ANTIGRAVITY"}}));
    }

    #[tokio::test]
    async fn g8_load_code_assist_object_and_alternate_keys() {
        for (load, expected) in [
            (
                r#"{"cloudaicompanionProject":{"id":" proj-obj ","name":"x"}}"#,
                "proj-obj",
            ),
            (
                r#"{"cloudaicompanionProject":"","projectId":"proj-alt"}"#,
                "proj-alt",
            ),
            (r#"{"project":{"id":"proj-last"}}"#, "proj-last"),
        ] {
            let (addr, _seen) = mock_server(routes(vec![(
                "/v1internal:loadCodeAssist",
                vec![(200, load)],
            )]))
            .await;
            let client = reqwest::Client::new();
            let project = resolve_project(&client, &format!("http://{addr}"), UA, "at")
                .await
                .expect("project");
            assert_eq!(project, expected, "{load}");
        }
    }

    // ---- G8: onboard fallback with the default tier, polled until done ----
    #[tokio::test]
    async fn g8_onboard_fallback_uses_the_default_tier_and_polls_until_done() {
        let (addr, seen) = mock_server(routes(vec![
            (
                "/v1internal:loadCodeAssist",
                vec![(
                    200,
                    r#"{"allowedTiers":[{"id":"legacy-tier"},{"id":"standard-tier","isDefault":true}],
                        "currentTier":{"id":"free-tier"}}"#,
                )],
            ),
            (
                "/v1internal:onboardUser",
                vec![
                    (200, r#"{"name":"op-1","done":false}"#),
                    (
                        200,
                        r#"{"name":"op-1","done":true,"response":{"cloudaicompanionProject":{"id":"proj-new"}}}"#,
                    ),
                ],
            ),
        ]))
        .await;
        let client = reqwest::Client::new();
        let project = resolve_project_with(
            &client,
            &format!("http://{addr}"),
            UA,
            "at-1",
            Duration::ZERO,
        )
        .await
        .expect("onboarded");
        assert_eq!(project, "proj-new");
        let seen = seen.lock().expect("lock").clone();
        let onboard: Vec<&Seen> = seen
            .iter()
            .filter(|s| s.path == "/v1internal:onboardUser")
            .collect();
        assert_eq!(onboard.len(), 2, "polled until done");
        assert_eq!(onboard[0].authorization, "Bearer at-1");
        assert_eq!(onboard[0].user_agent, UA);
        let body: Value = serde_json::from_str(&onboard[0].body).expect("json body");
        assert_eq!(
            body,
            json!({
                "tier_id": "standard-tier",
                "metadata": {"ide_type":"ANTIGRAVITY","ide_name":"antigravity","ide_version":"2.23.1"}
            })
        );
    }

    #[tokio::test]
    async fn g8_onboard_that_never_completes_is_an_error_after_five_polls() {
        let (addr, seen) = mock_server(routes(vec![
            ("/v1internal:loadCodeAssist", vec![(200, r#"{}"#)]),
            ("/v1internal:onboardUser", vec![(200, r#"{"done":false}"#)]),
        ]))
        .await;
        let client = reqwest::Client::new();
        let err =
            resolve_project_with(&client, &format!("http://{addr}"), UA, "at", Duration::ZERO)
                .await
                .expect_err("never done");
        assert!(matches!(err, AuthError::GeminiAuth(_)), "{err}");
        let onboard = seen
            .lock()
            .expect("lock")
            .iter()
            .filter(|s| s.path == "/v1internal:onboardUser")
            .count();
        assert_eq!(onboard, ONBOARD_MAX_ATTEMPTS);
    }

    #[tokio::test]
    async fn g8_load_code_assist_non_2xx_keeps_the_status() {
        let (addr, _seen) = mock_server(routes(vec![(
            "/v1internal:loadCodeAssist",
            vec![(401, r#"{"error":{"code":401,"status":"UNAUTHENTICATED"}}"#)],
        )]))
        .await;
        let client = reqwest::Client::new();
        let err = resolve_project(&client, &format!("http://{addr}"), UA, "at-secret")
            .await
            .expect_err("401");
        assert_eq!(err.http_status(), Some(StatusCode::UNAUTHORIZED));
        assert!(!err.to_string().contains("at-secret"), "no token: {err}");
    }

    #[test]
    fn g8_onboard_tier_falls_back_to_current_tier_then_free_tier() {
        assert_eq!(
            onboard_tier_id(&json!({"allowedTiers":[{"id":"","isDefault":true},
                {"id":"g1-plus-tier","isDefault":true}]})),
            "g1-plus-tier",
            "first default tier with a non-empty id"
        );
        assert_eq!(
            onboard_tier_id(&json!({"allowedTiers":[{"id":"legacy-tier"}],
                "currentTier":{"id":"free-tier-x"}})),
            "free-tier-x",
            "no default allowed tier → currentTier.id"
        );
        assert_eq!(onboard_tier_id(&json!({})), "free-tier");
    }

    #[test]
    fn ide_version_comes_from_the_configured_user_agent() {
        assert_eq!(ide_version("antigravity/hub/2.24.0 darwin/arm64"), "2.24.0");
        assert_eq!(ide_version("Antigravity/Hub/2.30.1 linux/amd64"), "2.30.1");
        assert_eq!(ide_version("antigravity/1.9.0 darwin/arm64"), "1.9.0");
        assert_eq!(ide_version("curl/8.0"), DEFAULT_IDE_VERSION);
        assert_eq!(ide_version(""), DEFAULT_IDE_VERSION);
        assert_eq!(ide_version("antigravity/hub/"), DEFAULT_IDE_VERSION);
    }

    // ---- G11: Antigravity CLI store import ----
    fn store_fixture(token: Value, id_token_payload: Option<Value>) -> Vec<u8> {
        let mut store = json!({"token": token, "auth_method": "oauth"});
        if let Some(payload) = id_token_payload {
            store["id_token"] = Value::String(fake_jwt(&payload));
        }
        store.to_string().into_bytes()
    }

    #[test]
    fn g11_store_parse_reads_email_and_fractional_rfc3339_expiry() {
        // Go's time.Time marshals fractional seconds + a numeric offset.
        let bytes = store_fixture(
            json!({"access_token":"ya29.at","token_type":"Bearer","refresh_token":"1//rt",
                "expiry":"2026-10-15T11:39:29.123456+09:00"}),
            Some(json!({"email":"a@example.com","aud":"test-client-id"})),
        );
        let account = account_from_antigravity_store(&bytes).expect("parse");
        assert_eq!(account.name, "gemini:a@example.com");
        let AccountCredential::Gemini {
            email,
            access_token,
            refresh_token,
            expires_at_ms,
            project_id,
            last_refresh_ms,
        } = account.credential
        else {
            panic!("gemini credential expected");
        };
        assert_eq!(email, "a@example.com");
        assert_eq!(access_token, "ya29.at");
        assert_eq!(refresh_token, "1//rt");
        // 2026-10-15T02:39:29.123456Z (`date -u -j -f …` → 1792031969 s).
        assert_eq!(expires_at_ms, 1_792_031_969_123);
        assert!(project_id.is_empty(), "resolved lazily on first request");
        assert_eq!(last_refresh_ms, None);
    }

    #[test]
    fn g11_store_without_parseable_expiry_is_unknown_zero() {
        let bytes = store_fixture(
            json!({"access_token":"at","refresh_token":"rt","expiry":"0001-01-01T00:00:00Z"}),
            Some(json!({"email":"a@example.com"})),
        );
        let account = account_from_antigravity_store(&bytes).expect("parse");
        let AccountCredential::Gemini { expires_at_ms, .. } = account.credential else {
            panic!("gemini credential expected");
        };
        assert_eq!(expires_at_ms, 0, "unknown → refresh on first use");
    }

    #[test]
    fn g11_store_missing_refresh_token_or_email_is_an_error() {
        let no_refresh = store_fixture(
            json!({"access_token":"at","expiry":"2026-10-15T02:39:29Z"}),
            Some(json!({"email":"a@example.com"})),
        );
        let err = account_from_antigravity_store(&no_refresh).expect_err("no refresh");
        assert!(
            matches!(err, AuthError::GeminiAuth("store missing refresh_token")),
            "{err}"
        );

        let no_email = store_fixture(
            json!({"access_token":"at","refresh_token":"rt"}),
            Some(json!({"sub":"123"})),
        );
        assert!(matches!(
            account_from_antigravity_store(&no_email),
            Err(AuthError::GeminiAuth(_))
        ));
        let no_id_token = store_fixture(json!({"access_token":"at","refresh_token":"rt"}), None);
        assert!(matches!(
            account_from_antigravity_store(&no_id_token),
            Err(AuthError::GeminiAuth(_))
        ));
        assert!(matches!(
            account_from_antigravity_store(br#"{"auth_method":"oauth"}"#),
            Err(AuthError::GeminiAuth(_))
        ));
        assert!(matches!(
            account_from_antigravity_store(b"not json"),
            Err(AuthError::Parse(_))
        ));
    }

    #[test]
    fn store_path_is_under_the_antigravity_cli_dir() {
        if let Some(path) = antigravity_store_path() {
            assert!(path.ends_with(".gemini/antigravity-cli/antigravity-oauth-token"));
        }
    }

    #[test]
    fn n1_initial_login_requires_a_refresh_token() {
        let mut tokens = OAuthTokens {
            access_token: "at".into(),
            refresh_token: None,
            expires_at_ms: 1,
        };
        assert!(login_refresh_token(&tokens).is_err());
        tokens.refresh_token = Some(String::new());
        assert!(login_refresh_token(&tokens).is_err());
        tokens.refresh_token = Some("rt".into());
        assert_eq!(login_refresh_token(&tokens).expect("present"), "rt");
    }

    #[test]
    fn account_name_and_error_body_cap() {
        assert_eq!(gemini_account_name("a@example.com"), "gemini:a@example.com");
        let long = "é".repeat(ERROR_BODY_CAP);
        let cut = capped(long);
        assert!(cut.len() <= ERROR_BODY_CAP + '…'.len_utf8());
        assert!(cut.ends_with('…'));
        assert_eq!(capped("short".into()), "short");
    }
}
