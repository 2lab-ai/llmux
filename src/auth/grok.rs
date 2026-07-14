//! xAI Grok subscription auth (docs/grok/spec.md §R1/§R2): OIDC discovery,
//! RFC 8628 device-code login, and refresh-token grants against the
//! discovered token endpoint. Ported from CLIProxyAPI's `internal/auth/xai`
//! (constants verified there; see the spec's evidence table). Unlike the
//! codex PKCE flow there is no localhost callback — the user opens a
//! verification URL and we poll.

use std::time::Duration;

use http::StatusCode;
use serde_json::Value;

use super::codex::{jwt_exp_ms, jwt_payload};
use super::oauth::{urlencode, OAuthTokens};
use super::AuthError;
use crate::config::{AccountConfig, AccountCredential};

/// xAI's OAuth issuer.
pub const GROK_ISSUER: &str = "https://auth.x.ai";

/// OIDC discovery endpoint that resolves the device + token endpoints.
pub const GROK_DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";

/// Public xAI Grok-CLI OAuth client id (CLIProxyAPI
/// `internal/auth/xai/types.go:19`).
pub const GROK_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

/// OAuth scope set required for xAI API access (types.go:21).
pub const GROK_SCOPES: &str = "openid profile email offline_access grok-cli:access api:access";

/// RFC 8628 device authorization grant type.
const DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Poll interval floor when the device endpoint omits `interval`.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Upper bound for waiting on user authorization.
const MAX_POLL_DURATION: Duration = Duration::from_secs(30 * 60);

/// Refresh retry policy, mirroring `codex::refresh_codex_at`.
const REFRESH_MAX_RETRIES: usize = 2;
const REFRESH_RETRY_DELAYS_MS: [u64; 3] = [500, 1000, 2000];

/// OAuth endpoints resolved from xAI OIDC discovery.
#[derive(Debug, Clone)]
pub struct GrokDiscovery {
    pub device_authorization_endpoint: String,
    pub token_endpoint: String,
}

/// xAI device authorization response (RFC 8628 §3.2).
#[derive(Debug, Clone)]
pub struct GrokDeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// `verification_uri_complete` when present (embeds the user code);
    /// empty when the endpoint omitted it — callers fall back to
    /// `verification_uri` + showing `user_code`.
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

impl GrokDeviceCode {
    /// The URL to open in a browser: the `_complete` variant when present,
    /// else the plain verification URI (the user then types `user_code`).
    pub fn open_url(&self) -> &str {
        if self.verification_uri_complete.is_empty() {
            &self.verification_uri
        } else {
            &self.verification_uri_complete
        }
    }
}

/// Validate an endpoint returned by xAI discovery: https and a hostname that
/// is exactly `x.ai` or a `.x.ai` label-boundary suffix (mirrors CLIProxyAPI
/// `ValidateOAuthEndpoint`, xai.go:47-64). Parsed with `reqwest::Url` — the
/// EXACT parser the outgoing request will use — so a crafted authority
/// (`https://evil.example\@auth.x.ai/…` and friends) cannot read differently
/// here than on the wire (external review round 2).
fn validate_endpoint(raw: &str, field: &'static str) -> Result<String, AuthError> {
    let raw = raw.trim();
    match parsed_https_host(raw) {
        Some(host) if host == "x.ai" || host.ends_with(".x.ai") => Ok(raw.to_string()),
        _ => Err(AuthError::GrokAuth(field)),
    }
}

/// Whether a persisted refresh endpoint may receive the refresh token:
/// the x.ai boundary (https) or loopback (http(s)://127.0.0.1 / localhost /
/// [::1] — used by tests and local mocks; loopback cannot exfiltrate
/// off-host).
fn refresh_endpoint_allowed(raw: &str) -> bool {
    if validate_endpoint(raw, "token_endpoint").is_ok() {
        return true;
    }
    let Ok(url) = reqwest::Url::parse(raw.trim()) else {
        return false;
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("[::1]")
    )
}

/// Hostname of an `https://` URL via `reqwest::Url` (lowercased by the
/// parser; userinfo and port never leak into the host). `None` for any other
/// scheme or an unparseable value.
fn parsed_https_host(raw: &str) -> Option<String> {
    let url = reqwest::Url::parse(raw).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    url.host_str().map(str::to_string)
}

/// A reqwest client for grok OAuth calls with redirects DISABLED (external
/// review round 3): the device-poll and refresh POSTs carry secrets (the
/// minted refresh token / the refresh token itself), and a 307/308 to an
/// off-boundary host would otherwise resend the body there. The daemon's
/// shared client is already `Policy::none()` (server.rs); this is the
/// constructor the CLI paths use so every grok token call is redirect-safe.
/// FAILS CLOSED (external review round 4): `ClientBuilder::build` only errors
/// on a static TLS/resolver init fault, never at runtime — but a security
/// control must never silently degrade to a redirect-FOLLOWING client, so a
/// build failure aborts rather than falls back to the default policy.
pub fn oauth_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("no-redirect reqwest client for grok OAuth (fail closed)")
}

/// Resolve the xAI OAuth endpoints via OIDC discovery.
pub async fn discover(client: &reqwest::Client) -> Result<GrokDiscovery, AuthError> {
    let response = client
        .get(GROK_DISCOVERY_URL)
        .header(http::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|err| AuthError::Network(err.to_string()))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| AuthError::Network(err.to_string()))?;
    if !status.is_success() {
        return Err(AuthError::TokenEndpoint { status, body: text });
    }
    let value: Value = serde_json::from_str(&text)?;
    let device = value
        .get("device_authorization_endpoint")
        .and_then(Value::as_str)
        .unwrap_or("");
    let token = value
        .get("token_endpoint")
        .and_then(Value::as_str)
        .unwrap_or("");
    Ok(GrokDiscovery {
        device_authorization_endpoint: validate_endpoint(
            device,
            "discovery device_authorization_endpoint invalid",
        )?,
        token_endpoint: validate_endpoint(token, "discovery token_endpoint invalid")?,
    })
}

/// Request a device authorization code (RFC 8628 §3.1: `client_id` + `scope`).
pub async fn request_device_code(
    client: &reqwest::Client,
    discovery: &GrokDiscovery,
) -> Result<GrokDeviceCode, AuthError> {
    let form_body = format!(
        "client_id={}&scope={}",
        urlencode(GROK_CLIENT_ID),
        urlencode(GROK_SCOPES),
    );
    let response = client
        .post(&discovery.device_authorization_endpoint)
        .header(http::header::ACCEPT, "application/json")
        .header(
            http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(form_body)
        .send()
        .await
        .map_err(|err| AuthError::Network(err.to_string()))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| AuthError::Network(err.to_string()))?;
    if !status.is_success() {
        return Err(AuthError::TokenEndpoint { status, body: text });
    }
    let value: Value = serde_json::from_str(&text)?;
    let device_code = GrokDeviceCode {
        device_code: str_field(&value, "device_code"),
        user_code: str_field(&value, "user_code"),
        verification_uri: str_field(&value, "verification_uri"),
        verification_uri_complete: str_field(&value, "verification_uri_complete"),
        expires_in: value.get("expires_in").and_then(Value::as_u64).unwrap_or(0),
        interval: value.get("interval").and_then(Value::as_u64).unwrap_or(0),
    };
    if device_code.device_code.is_empty() {
        return Err(AuthError::GrokAuth("device response missing device_code"));
    }
    if device_code.user_code.is_empty() {
        return Err(AuthError::GrokAuth("device response missing user_code"));
    }
    if device_code.verification_uri.is_empty() && device_code.verification_uri_complete.is_empty() {
        return Err(AuthError::GrokAuth(
            "device response missing verification URI",
        ));
    }
    Ok(device_code)
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Tokens minted by a completed device-code login. `id_token` carries the
/// identity claims (email / sub) for the account name.
#[derive(Debug, Clone)]
pub struct GrokTokenBundle {
    pub tokens: OAuthTokens,
    pub id_token: Option<String>,
}

/// Poll the token endpoint until the user authorizes the device code
/// (RFC 8628 §3.4-3.5): `authorization_pending` → keep polling, `slow_down` →
/// interval + 5s, `expired_token` / `access_denied` → terminal. The deadline
/// is min(30 min, `expires_in`).
pub async fn poll_token(
    client: &reqwest::Client,
    token_endpoint: &str,
    device_code: &GrokDeviceCode,
) -> Result<GrokTokenBundle, AuthError> {
    let mut interval = Duration::from_secs(device_code.interval).max(DEFAULT_POLL_INTERVAL);
    let mut deadline = MAX_POLL_DURATION;
    if device_code.expires_in > 0 {
        deadline = deadline.min(Duration::from_secs(device_code.expires_in));
    }
    let started = std::time::Instant::now();
    let form_body = format!(
        "grant_type={}&device_code={}&client_id={}",
        urlencode(DEVICE_CODE_GRANT_TYPE),
        urlencode(&device_code.device_code),
        urlencode(GROK_CLIENT_ID),
    );

    loop {
        let response = client
            .post(token_endpoint)
            .header(http::header::ACCEPT, "application/json")
            .header(
                http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form_body.clone())
            .send()
            .await
            .map_err(|err| AuthError::Network(err.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|err| AuthError::Network(err.to_string()))?;
        let value: Value = serde_json::from_str(&text)?;

        match value.get("error").and_then(Value::as_str).unwrap_or("") {
            "" => {
                if !status.is_success() {
                    return Err(AuthError::TokenEndpoint { status, body: text });
                }
                return parse_grok_token_response(&value);
            }
            "authorization_pending" => {}
            "slow_down" => interval += DEFAULT_POLL_INTERVAL,
            "expired_token" => {
                return Err(AuthError::Aborted("grok device code expired".into()));
            }
            "access_denied" => {
                return Err(AuthError::Aborted(
                    "grok device authorization denied".into(),
                ));
            }
            other => {
                return Err(AuthError::Aborted(format!(
                    "grok device token error: {other}"
                )));
            }
        }
        if started.elapsed() + interval > deadline {
            return Err(AuthError::Aborted("grok device code expired".into()));
        }
        tokio::time::sleep(interval).await;
    }
}

/// Parse an xAI token response into [`GrokTokenBundle`]. Expiry preference:
/// the access token's JWT `exp` claim, else `expires_in`, else a 1h floor
/// (same policy as the codex parser).
fn parse_grok_token_response(value: &Value) -> Result<GrokTokenBundle, AuthError> {
    let access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(AuthError::GrokAuth("token response missing access_token"))?
        .to_string();
    let refresh_token = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let expires_at_ms = jwt_exp_ms(&access_token).unwrap_or_else(|| {
        let expires_in = value
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(3600);
        now_ms() + expires_in.saturating_mul(1000)
    });
    let id_token = value
        .get("id_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(GrokTokenBundle {
        tokens: OAuthTokens {
            access_token,
            refresh_token,
            expires_at_ms,
        },
        id_token,
    })
}

/// One Grok token refresh: POST form-encoded
/// `grant_type=refresh_token&refresh_token=…&client_id=…` to
/// `token_endpoint` (re-discovered when empty — the persisted endpoint can
/// be blank on hand-edited configs). Same retry taxonomy as codex:
/// 5xx/network retried through the ladder; 401 or `invalid_grant` is
/// [`AuthError::RefreshPermanent`] (re-login required).
pub async fn refresh_grok_at(
    client: &reqwest::Client,
    token_endpoint: &str,
    refresh_token: &str,
) -> Result<OAuthTokens, AuthError> {
    // Validate on EVERY use, not just at discovery (external review
    // MUST-FIX, 2026-07-14): the persisted endpoint comes from the config
    // file and would otherwise receive the refresh token wherever it points.
    // A non-x.ai persisted value falls back to fresh discovery instead of
    // being trusted. Loopback stays allowed — local mocks/tests can't
    // exfiltrate off-host, and whoever controls loopback controls the host.
    let trimmed = token_endpoint.trim();
    let token_endpoint = if trimmed.is_empty() {
        discover(client).await?.token_endpoint
    } else if refresh_endpoint_allowed(trimmed) {
        trimmed.to_string()
    } else {
        tracing::warn!(
            endpoint = trimmed,
            "grok: persisted token_endpoint failed x.ai validation; re-discovering"
        );
        discover(client).await?.token_endpoint
    };
    let form_body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        urlencode(refresh_token),
        urlencode(GROK_CLIENT_ID),
    );

    let mut attempt = 0usize;
    loop {
        if attempt > 0 {
            let index = (attempt - 1).min(REFRESH_RETRY_DELAYS_MS.len() - 1);
            tokio::time::sleep(Duration::from_millis(REFRESH_RETRY_DELAYS_MS[index])).await;
        }

        let response = match client
            .post(&token_endpoint)
            .header(http::header::ACCEPT, "application/json")
            .header(
                http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form_body.clone())
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
            let value: Value = serde_json::from_str(&text)?;
            return parse_grok_token_response(&value).map(|bundle| bundle.tokens);
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

/// Identity claims from an id_token: `(email, sub)`, either may be empty.
pub fn grok_identity(id_token: Option<&str>) -> (String, String) {
    let Some(claims) = id_token.and_then(jwt_payload) else {
        return (String::new(), String::new());
    };
    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let subject = claims
        .get("sub")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    (email, subject)
}

/// Account name for a grok credential: `grok:{email}`, falling back to the
/// JWT `sub`, then an epoch-ms stamp (mirrors CLIProxyAPI
/// `CredentialFileName`, token.go:71-81).
pub fn grok_account_name(email: &str, subject: &str) -> String {
    if !email.is_empty() {
        return format!("grok:{email}");
    }
    if !subject.is_empty() {
        return format!("grok:{subject}");
    }
    format!("grok:{}", now_ms())
}

/// Build the persistable account from a completed device-code login. An
/// initial login WITHOUT a refresh token is an error (external review N1):
/// the account would silently die at first access-token expiry. (Refresh
/// responses omitting the field keep the stored token — different contract,
/// see `refresh_grok_at` / C8.)
pub fn account_from_bundle(
    bundle: &GrokTokenBundle,
    token_endpoint: &str,
) -> Result<AccountConfig, AuthError> {
    let Some(refresh_token) = bundle
        .tokens
        .refresh_token
        .clone()
        .filter(|t| !t.is_empty())
    else {
        return Err(AuthError::GrokAuth(
            "login token response missing refresh_token (offline_access not granted?)",
        ));
    };
    let (email, subject) = grok_identity(bundle.id_token.as_deref());
    Ok(AccountConfig {
        name: grok_account_name(&email, &subject),
        credential: AccountCredential::Grok {
            subject,
            access_token: bundle.tokens.access_token.clone(),
            refresh_token,
            expires_at_ms: bundle.tokens.expires_at_ms,
            token_endpoint: token_endpoint.to_string(),
            last_refresh_ms: Some(now_ms()),
        },
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_endpoint_enforces_https_and_xai_host() {
        assert!(validate_endpoint("https://auth.x.ai/oauth/token", "f").is_ok());
        assert!(validate_endpoint("https://x.ai/token", "f").is_ok());
        // label-boundary: evil-x.ai is NOT .x.ai
        assert!(validate_endpoint("https://evil-x.ai/token", "f").is_err());
        assert!(validate_endpoint("http://auth.x.ai/token", "f").is_err());
        assert!(validate_endpoint("https://auth.example.com/token", "f").is_err());
        assert!(validate_endpoint("", "f").is_err());
        // Authority-confusion vectors (external review round 2/3): assert the
        // EXACT host reqwest::Url resolves AND the validation result, so a
        // future parser behavior change fails this test loudly rather than
        // silently widening the boundary.
        let backslash = r"https://evil.example\@auth.x.ai/token";
        let parsed_host = reqwest::Url::parse(backslash)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));
        // Whatever host it parses to, validation agrees: allowed IFF the host
        // is on the x.ai boundary.
        let on_boundary = parsed_host
            .as_deref()
            .is_some_and(|h| h == "x.ai" || h.ends_with(".x.ai"));
        assert_eq!(
            validate_endpoint(backslash, "f").is_ok(),
            on_boundary,
            "validation must track the transport's parsed host {parsed_host:?}"
        );
        assert!(validate_endpoint("https://auth.x.ai@evil.example/token", "f").is_err());
        assert!(validate_endpoint("https://auth.x.ai.evil.example/token", "f").is_err());
    }

    #[test]
    fn account_name_prefers_email_then_subject() {
        assert_eq!(grok_account_name("a@b.c", "sub1"), "grok:a@b.c");
        assert_eq!(grok_account_name("", "sub1"), "grok:sub1");
        assert!(grok_account_name("", "").starts_with("grok:"));
    }

    #[test]
    fn device_code_open_url_falls_back_to_plain_uri() {
        let mut dc = GrokDeviceCode {
            device_code: "d".into(),
            user_code: "ABCD-EFGH".into(),
            verification_uri: "https://x.ai/device".into(),
            verification_uri_complete: "https://x.ai/device?code=ABCD-EFGH".into(),
            expires_in: 600,
            interval: 5,
        };
        assert_eq!(dc.open_url(), "https://x.ai/device?code=ABCD-EFGH");
        dc.verification_uri_complete.clear();
        assert_eq!(dc.open_url(), "https://x.ai/device");
    }

    use serde_json::json;
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

    /// One-route mock server; returns (addr, seen request bodies).
    async fn mock_token_server(
        responses: Vec<(u16, String)>,
    ) -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_in_handler = Arc::clone(&seen);
        let responses = Arc::new(Mutex::new(responses));
        let app = axum::Router::new().route(
            "/token",
            axum::routing::post(move |req: axum::extract::Request| {
                let seen = Arc::clone(&seen_in_handler);
                let responses = Arc::clone(&responses);
                async move {
                    let bytes = axum::body::to_bytes(req.into_body(), usize::MAX)
                        .await
                        .expect("body");
                    seen.lock()
                        .expect("lock")
                        .push(String::from_utf8_lossy(&bytes).into_owned());
                    let (status, body) = {
                        let mut r = responses.lock().expect("lock");
                        if r.len() > 1 {
                            r.remove(0)
                        } else {
                            r[0].clone()
                        }
                    };
                    (
                        http::StatusCode::from_u16(status).expect("status"),
                        [(http::header::CONTENT_TYPE, "application/json")],
                        body,
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, seen)
    }

    fn device_code_fixture() -> GrokDeviceCode {
        GrokDeviceCode {
            device_code: "dev-1".into(),
            user_code: "ABCD-EFGH".into(),
            verification_uri: "https://x.ai/device".into(),
            verification_uri_complete: String::new(),
            expires_in: 600,
            // 0 → clamped to the 5s floor; tests only need few iterations.
            interval: 0,
        }
    }

    // ---- C5: device-code poll happy path (pending → success), form fields ----
    #[tokio::test]
    async fn c5_poll_pends_then_succeeds_with_client_id_in_form() {
        let access = fake_jwt(&json!({"exp": 1_900_000_000}));
        let (addr, seen) = mock_token_server(vec![
            (400, r#"{"error":"authorization_pending"}"#.to_string()),
            (
                200,
                format!(r#"{{"access_token":"{access}","refresh_token":"rt-g1","id_token":"x.y.z","expires_in":3600}}"#),
            ),
        ])
        .await;
        let client = reqwest::Client::new();
        let bundle = poll_token(
            &client,
            &format!("http://{addr}/token"),
            &device_code_fixture(),
        )
        .await
        .expect("poll succeeds");
        assert_eq!(bundle.tokens.access_token, access);
        assert_eq!(bundle.tokens.refresh_token.as_deref(), Some("rt-g1"));
        assert_eq!(bundle.tokens.expires_at_ms, 1_900_000_000_000);
        let seen = seen.lock().expect("lock").clone();
        assert!(seen.len() >= 2, "polled at least twice");
        assert!(
            seen[0].contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"),
            "device grant type: {}",
            seen[0]
        );
        assert!(seen[0].contains("device_code=dev-1"), "{}", seen[0]);
        assert!(
            seen[0].contains(&format!("client_id={GROK_CLIENT_ID}")),
            "client_id REQUIRED in poll form (spec §R1): {}",
            seen[0]
        );
    }

    // ---- C6: access_denied is terminal ----
    #[tokio::test]
    async fn c6_poll_access_denied_is_terminal() {
        let (addr, _seen) =
            mock_token_server(vec![(400, r#"{"error":"access_denied"}"#.to_string())]).await;
        let client = reqwest::Client::new();
        let err = poll_token(
            &client,
            &format!("http://{addr}/token"),
            &device_code_fixture(),
        )
        .await
        .expect_err("denied");
        assert!(err.to_string().contains("denied"), "{err}");
    }

    // ---- C8: refresh form + rotation semantics + invalid_grant ----
    #[tokio::test]
    async fn c8_refresh_rotates_when_response_carries_new_token() {
        let access = fake_jwt(&json!({"exp": 1_900_000_000}));
        let (addr, seen) = mock_token_server(vec![(
            200,
            format!(r#"{{"access_token":"{access}","refresh_token":"rt-rotated"}}"#),
        )])
        .await;
        let client = reqwest::Client::new();
        let tokens = refresh_grok_at(&client, &format!("http://{addr}/token"), "rt-old")
            .await
            .expect("refresh");
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt-rotated"));
        let seen = seen.lock().expect("lock").clone();
        assert!(seen[0].contains("grant_type=refresh_token"), "{}", seen[0]);
        assert!(seen[0].contains("refresh_token=rt-old"), "{}", seen[0]);
        assert!(
            seen[0].contains(&format!("client_id={GROK_CLIENT_ID}")),
            "client_id REQUIRED in refresh form: {}",
            seen[0]
        );
    }

    #[tokio::test]
    async fn c8_refresh_without_new_token_keeps_none_for_caller_preserve() {
        let access = fake_jwt(&json!({"exp": 1_900_000_000}));
        let (addr, _seen) =
            mock_token_server(vec![(200, format!(r#"{{"access_token":"{access}"}}"#))]).await;
        let client = reqwest::Client::new();
        let tokens = refresh_grok_at(&client, &format!("http://{addr}/token"), "rt-keep")
            .await
            .expect("refresh");
        assert!(
            tokens.refresh_token.is_none(),
            "None → forward.rs keeps the stored refresh token (C8)"
        );
    }

    #[tokio::test]
    async fn c8_refresh_invalid_grant_is_permanent() {
        let (addr, _seen) = mock_token_server(vec![(
            400,
            r#"{"error":"invalid_grant","error_description":"dead"}"#.to_string(),
        )])
        .await;
        let client = reqwest::Client::new();
        let err = refresh_grok_at(&client, &format!("http://{addr}/token"), "rt-dead")
            .await
            .expect_err("permanent");
        assert!(
            matches!(err, AuthError::RefreshPermanent { .. }),
            "invalid_grant → Permanent (re-login required): {err}"
        );
    }

    #[test]
    fn refresh_endpoint_validation_gates_every_use() {
        assert!(refresh_endpoint_allowed("https://auth.x.ai/oauth/token"));
        assert!(refresh_endpoint_allowed("http://127.0.0.1:3498/token"));
        assert!(refresh_endpoint_allowed("http://localhost:8080/token"));
        assert!(refresh_endpoint_allowed("http://[::1]:8080/token"));
        assert!(!refresh_endpoint_allowed("https://evil.example.com/token"));
        assert!(!refresh_endpoint_allowed("https://evil-x.ai/token"));
        assert!(!refresh_endpoint_allowed("http://10.0.0.5/token"));
        assert!(!refresh_endpoint_allowed("ftp://127.0.0.1/token"));
        assert!(!refresh_endpoint_allowed(
            "http://localhost.evil.example/token"
        ));
    }

    #[test]
    fn n1_initial_login_requires_refresh_token() {
        let bundle = GrokTokenBundle {
            tokens: OAuthTokens {
                access_token: "at".into(),
                refresh_token: None,
                expires_at_ms: 1,
            },
            id_token: None,
        };
        assert!(account_from_bundle(&bundle, "https://auth.x.ai/t").is_err());
        let ok = GrokTokenBundle {
            tokens: OAuthTokens {
                access_token: "at".into(),
                refresh_token: Some("rt".into()),
                expires_at_ms: 1,
            },
            id_token: None,
        };
        assert!(account_from_bundle(&ok, "https://auth.x.ai/t").is_ok());
    }

    #[test]
    fn token_response_parses_and_keeps_missing_refresh_none() {
        let value: Value = serde_json::from_str(
            r#"{"access_token":"not-a-jwt","expires_in":7200,"id_token":"x.y.z"}"#,
        )
        .unwrap();
        let bundle = parse_grok_token_response(&value).unwrap();
        assert_eq!(bundle.tokens.access_token, "not-a-jwt");
        assert!(bundle.tokens.refresh_token.is_none());
        assert!(bundle.tokens.expires_at_ms > now_ms() + 7_000_000);
    }
}
