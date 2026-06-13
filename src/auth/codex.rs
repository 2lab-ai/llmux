//! OpenAI Codex (ChatGPT subscription) auth: `~/.codex/auth.json` import,
//! JWT claim decoding (no signature verification — we only need `exp` and
//! the email for display), and refresh-token grants against
//! `https://auth.openai.com/oauth/token` (form-encoded, unlike Anthropic's
//! JSON token endpoint).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use http::StatusCode;
use serde_json::Value;

use super::oauth::{self, OAuthTokens, PkcePair};
use super::AuthError;
use crate::config::{AccountConfig, AccountCredential};

/// OAuth client id the codex CLI uses for refresh-token grants AND the
/// interactive ChatGPT login (one client id for both).
pub const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Browser authorization endpoint for the ChatGPT subscription login.
/// (Verified against openai/codex @ f297b9f: issuer `https://auth.openai.com`.)
pub const OPENAI_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";

/// Scope string for the ChatGPT login, verbatim from the codex CLI.
pub const OPENAI_SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";

/// Preferred loopback callback ports, in fallback order (1455, then 1457).
/// The codex CLI binds 1455 and the provider has it registered; 1457 is the
/// documented fallback when 1455 is taken.
pub const CODEX_CALLBACK_PORTS: [u16; 2] = [1455, 1457];

/// The browser lands on `http://localhost:{port}/auth/callback` after a
/// successful ChatGPT authorization.
const CODEX_CALLBACK_PATH: &str = "/auth/callback";

/// Refresh retry policy, mirroring `oauth::refresh_at`: up to 2 retries on
/// 5xx / network errors.
const REFRESH_MAX_RETRIES: usize = 2;
const REFRESH_RETRY_DELAYS_MS: [u64; 3] = [500, 1000, 2000];

/// Default location of the codex CLI credential store.
pub fn default_codex_auth_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".codex").join("auth.json"))
}

/// Decode a JWT's payload segment without verifying the signature (we are
/// the token holder; only the claims matter). Tolerates missing padding.
pub fn jwt_payload(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// `exp` claim of a JWT, converted to epoch milliseconds. `None` when the
/// token is not a JWT or carries no usable `exp`.
pub fn jwt_exp_ms(token: &str) -> Option<u64> {
    let exp = jwt_payload(token)?.get("exp")?.as_u64()?;
    Some(exp.saturating_mul(1000))
}

/// Best-effort email from an OpenAI id_token: top-level `email` claim or the
/// `https://api.openai.com/profile` object's `email`.
pub fn jwt_email(token: &str) -> Option<String> {
    let payload = jwt_payload(token)?;
    if let Some(email) = payload.get("email").and_then(Value::as_str) {
        return Some(email.to_string());
    }
    payload
        .get("https://api.openai.com/profile")
        .and_then(|profile| profile.get("email"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// ChatGPT account id from an OpenAI id_token: the `chatgpt_account_id` field
/// nested under the `https://api.openai.com/auth` claim object. This is the
/// dedup key for an OAuth-minted Codex account (parity with `auth.json`'s
/// `tokens.account_id`).
pub fn jwt_account_id(token: &str) -> Option<String> {
    jwt_payload(token)?
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Parse the contents of `~/.codex/auth.json`:
///
/// ```json
/// { "auth_mode": ..., "OPENAI_API_KEY": null,
///   "tokens": { "id_token", "access_token", "refresh_token", "account_id" },
///   "last_refresh": ... }
/// ```
///
/// Returns one account named after the id_token email when decodable,
/// `"codex"` otherwise; `expires_at_ms` is derived from the access token's
/// JWT `exp` claim (0 when undecodable — the proxy then refreshes on first
/// use).
pub fn parse_codex_auth(raw: &str) -> Result<AccountConfig, AuthError> {
    let value: Value = serde_json::from_str(raw)?;
    let tokens = value
        .get("tokens")
        .and_then(Value::as_object)
        .ok_or(AuthError::CodexAuth("missing \"tokens\" object"))?;
    let field = |name: &'static str| -> Option<String> {
        tokens
            .get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let access_token = field("access_token").ok_or(AuthError::CodexAuth("missing access_token"))?;
    let refresh_token =
        field("refresh_token").ok_or(AuthError::CodexAuth("missing refresh_token"))?;
    let account_id = field("account_id").ok_or(AuthError::CodexAuth("missing account_id"))?;
    let name = field("id_token")
        .and_then(|id_token| jwt_email(&id_token))
        .unwrap_or_else(|| "codex".to_string());
    let expires_at_ms = jwt_exp_ms(&access_token).unwrap_or(0);
    Ok(AccountConfig {
        name,
        credential: AccountCredential::Codex {
            account_id,
            access_token,
            refresh_token,
            expires_at_ms,
            // auth.json's "last_refresh" is an ISO string; parsing it would
            // pull in a date dependency for a display nicety — imported
            // tokens show "never (refreshed)" until the proxy's first
            // refresh stamps the field.
            last_refresh_ms: None,
        },
    })
}

/// [`parse_codex_auth`] over a file path.
pub fn import_codex_auth(path: &Path) -> Result<AccountConfig, AuthError> {
    let raw = std::fs::read_to_string(path)?;
    parse_codex_auth(&raw)
}

/// One Codex token refresh: POST form-encoded
/// `grant_type=refresh_token&refresh_token=...&client_id=...` to
/// `token_url`. Same retry taxonomy as the Anthropic refresh:
/// 5xx/network retried through the ladder; 401 or `invalid_grant` is
/// [`AuthError::RefreshPermanent`] (re-import required).
pub async fn refresh_codex_at(
    client: &reqwest::Client,
    token_url: &str,
    refresh_token: &str,
) -> Result<OAuthTokens, AuthError> {
    // Form-encoded by hand: reqwest's `.form()` helper sits behind a cargo
    // feature this crate does not enable; the body is three known fields.
    let form_body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        super::oauth::urlencode(refresh_token),
        OPENAI_CLIENT_ID,
    );

    let mut attempt = 0usize;
    loop {
        if attempt > 0 {
            let index = (attempt - 1).min(REFRESH_RETRY_DELAYS_MS.len() - 1);
            tokio::time::sleep(Duration::from_millis(REFRESH_RETRY_DELAYS_MS[index])).await;
        }

        let response = match client
            .post(token_url)
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
            return parse_codex_token_response(&text);
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

/// Parse the OpenAI token response. Expiry preference: the new access
/// token's JWT `exp` claim (authoritative), else `expires_in`, else a 1h
/// floor — the proxy refreshes 5 minutes ahead of whatever this says.
fn parse_codex_token_response(text: &str) -> Result<OAuthTokens, AuthError> {
    let value: Value = serde_json::from_str(text)?;
    let access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(AuthError::CodexAuth("token response missing access_token"))?
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
    Ok(OAuthTokens {
        access_token,
        refresh_token,
        expires_at_ms,
    })
}

/// Build the ChatGPT authorize URL (PKCE S256). Query params and their order
/// mirror the codex CLI exactly (verified @ f297b9f); values are percent-
/// encoded via the shared `oauth::urlencode`.
pub fn codex_authorize_url(pkce: &PkcePair, state: &str, redirect_uri: &str) -> String {
    let params: [(&str, &str); 10] = [
        ("response_type", "code"),
        ("client_id", OPENAI_CLIENT_ID),
        ("redirect_uri", redirect_uri),
        ("scope", OPENAI_SCOPES),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
        ("originator", "codex_cli_rs"),
    ];
    let mut url = String::from(OPENAI_AUTHORIZE_URL);
    for (i, (key, value)) in params.iter().enumerate() {
        url.push(if i == 0 { '?' } else { '&' });
        url.push_str(key);
        url.push('=');
        url.push_str(&oauth::urlencode(value));
    }
    url
}

/// Outcome of a Codex authorization-code exchange: the parsed tokens plus the
/// raw `id_token` (needed for the email display name and account-id dedup key,
/// neither of which is in the access token).
struct CodexCodeExchange {
    tokens: OAuthTokens,
    id_token: Option<String>,
}

/// Exchange a ChatGPT authorization code for tokens. The OpenAI token endpoint
/// is FORM-URLENCODED (unlike Anthropic's JSON exchange) and takes the PKCE
/// `code_verifier` with NO client_secret.
async fn exchange_codex_code(
    client: &reqwest::Client,
    token_url: &str,
    code: &str,
    redirect_uri: &str,
    pkce: &PkcePair,
) -> Result<CodexCodeExchange, AuthError> {
    let form_body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        oauth::urlencode(code),
        oauth::urlencode(redirect_uri),
        OPENAI_CLIENT_ID,
        oauth::urlencode(&pkce.verifier),
    );

    let response = client
        .post(token_url)
        .header(http::header::ACCEPT, "application/json")
        .header(
            http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(form_body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::TokenEndpoint { status, body });
    }
    let text = response.text().await?;
    let id_token = serde_json::from_str::<Value>(&text).ok().and_then(|v| {
        v.get("id_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });
    let tokens = parse_codex_token_response(&text)?;
    Ok(CodexCodeExchange { tokens, id_token })
}

/// Full interactive ChatGPT login: bind the fixed loopback callback (1455,
/// fallback 1457), open the browser to the OpenAI authorize URL, race the
/// callback against manual paste, then form-urlencode-exchange the code.
/// Returns a ready-to-upsert `AccountConfig` named `codex:{email}`.
///
/// `token_url` is the configured Codex token endpoint
/// (`config.codex.token_url`, default `https://auth.openai.com/oauth/token`).
pub async fn login_codex_interactive(
    client: &reqwest::Client,
    token_url: &str,
) -> Result<AccountConfig, AuthError> {
    let pkce = PkcePair::generate();
    let state = oauth::random_b64url_32();

    let (listener, port) = oauth::bind_callback_listener(&CODEX_CALLBACK_PORTS).await?;
    // redirect_uri hostname is `localhost` (the provider's registered value),
    // even though the listener binds 127.0.0.1.
    let redirect_uri = format!("http://localhost:{port}{CODEX_CALLBACK_PATH}");

    let url = codex_authorize_url(&pkce, &state, &redirect_uri);
    eprintln!("Opening browser for ChatGPT authentication...");
    eprintln!("If it doesn't open, visit:\n  {url}\n");
    oauth::open_browser(&url);

    let code = oauth::run_callback_server(listener, CODEX_CALLBACK_PATH, &state).await?;
    let exchange = exchange_codex_code(client, token_url, &code, &redirect_uri, &pkce).await?;

    account_from_exchange(exchange)
}

/// Turn a completed code exchange into an `AccountConfig`. account_id comes
/// from the id_token's `chatgpt_account_id` claim (dedup key); name is
/// `codex:{email}`, falling back to `codex:{account_id-prefix}` when the
/// email is undecodable. Stamps `last_refresh_ms = now` (the fresh exchange
/// IS a refresh for the dashboard).
fn account_from_exchange(exchange: CodexCodeExchange) -> Result<AccountConfig, AuthError> {
    let CodexCodeExchange { tokens, id_token } = exchange;
    let id_token = id_token
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or(AuthError::CodexAuth("token response missing id_token"))?;

    let account_id = jwt_account_id(id_token)
        .ok_or(AuthError::CodexAuth("id_token missing chatgpt_account_id"))?;
    let refresh_token = tokens
        .refresh_token
        .filter(|s| !s.is_empty())
        .ok_or(AuthError::CodexAuth("token response missing refresh_token"))?;

    let name = codex_account_name(jwt_email(id_token).as_deref(), &account_id);

    Ok(AccountConfig {
        name,
        credential: AccountCredential::Codex {
            account_id,
            access_token: tokens.access_token,
            refresh_token,
            expires_at_ms: tokens.expires_at_ms,
            last_refresh_ms: Some(now_ms()),
        },
    })
}

/// Account name encoding the model group so the same email can live as both a
/// Claude and a Codex account: `codex:{email}`, or `codex:{account_id-prefix}`
/// when the email is unknown.
pub fn codex_account_name(email: Option<&str>, account_id: &str) -> String {
    match email.filter(|s| !s.is_empty()) {
        Some(email) => format!("codex:{email}"),
        None => {
            let prefix: String = account_id.chars().take(8).collect();
            format!("codex:{prefix}")
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unsigned fake JWT with the given JSON payload (tests only — the
    /// decoder never checks signatures).
    pub(crate) fn fake_jwt(payload: &Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let body = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        format!("{header}.{body}.sig")
    }

    #[test]
    fn jwt_exp_extraction() {
        let token = fake_jwt(&serde_json::json!({"exp": 1_750_000_000, "sub": "x"}));
        assert_eq!(jwt_exp_ms(&token), Some(1_750_000_000_000));
    }

    #[test]
    fn jwt_exp_missing_or_garbage_is_none() {
        assert_eq!(jwt_exp_ms("not-a-jwt"), None);
        assert_eq!(jwt_exp_ms("a.b.c"), None);
        let token = fake_jwt(&serde_json::json!({"sub": "x"}));
        assert_eq!(jwt_exp_ms(&token), None);
        let token = fake_jwt(&serde_json::json!({"exp": "soon"}));
        assert_eq!(jwt_exp_ms(&token), None);
    }

    #[test]
    fn jwt_email_top_level_and_profile_claim() {
        let token = fake_jwt(&serde_json::json!({"email": "z@example.com"}));
        assert_eq!(jwt_email(&token), Some("z@example.com".into()));
        let token = fake_jwt(&serde_json::json!({
            "https://api.openai.com/profile": {"email": "p@example.com"}
        }));
        assert_eq!(jwt_email(&token), Some("p@example.com".into()));
        let token = fake_jwt(&serde_json::json!({"sub": "x"}));
        assert_eq!(jwt_email(&token), None);
    }

    #[test]
    fn jwt_account_id_from_auth_claim() {
        let token = fake_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct-123"}
        }));
        assert_eq!(jwt_account_id(&token), Some("acct-123".into()));
        // Missing claim / wrong shape / not-a-jwt → None.
        let token = fake_jwt(&serde_json::json!({"sub": "x"}));
        assert_eq!(jwt_account_id(&token), None);
        assert_eq!(jwt_account_id("opaque"), None);
    }

    #[test]
    fn codex_account_name_prefixes_group() {
        assert_eq!(
            codex_account_name(Some("icedac@gmail.com"), "acct-uuid"),
            "codex:icedac@gmail.com"
        );
        // No email → codex:{account_id-prefix} (first 8 chars).
        assert_eq!(codex_account_name(None, "abcdefghIJKL"), "codex:abcdefgh");
        // Empty email is treated as absent.
        assert_eq!(codex_account_name(Some(""), "acct-2xy"), "codex:acct-2xy");
    }

    #[test]
    fn codex_authorize_url_matches_codex_cli_params() {
        let pkce = PkcePair::generate();
        let url = codex_authorize_url(&pkce, "the-state", "http://localhost:1455/auth/callback");
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?response_type=code&"));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        assert!(url.contains(
            "scope=openid%20profile%20email%20offline_access%20\
             api.connectors.read%20api.connectors.invoke"
        ));
        assert!(url.contains(&format!("code_challenge={}", pkce.challenge)));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("id_token_add_organizations=true"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("state=the-state"));
        assert!(url.contains("originator=codex_cli_rs"));
    }

    #[test]
    fn account_from_exchange_builds_codex_account() {
        let id_token = fake_jwt(&serde_json::json!({
            "email": "icedac@gmail.com",
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct-abc"}
        }));
        let exchange = CodexCodeExchange {
            tokens: OAuthTokens {
                access_token: "at-fresh".into(),
                refresh_token: Some("rt-fresh".into()),
                expires_at_ms: 1_900_000_000_000,
            },
            id_token: Some(id_token),
        };
        let account = account_from_exchange(exchange).expect("account");
        assert_eq!(account.name, "codex:icedac@gmail.com");
        match account.credential {
            AccountCredential::Codex {
                account_id,
                access_token,
                refresh_token,
                expires_at_ms,
                last_refresh_ms,
            } => {
                assert_eq!(account_id, "acct-abc");
                assert_eq!(access_token, "at-fresh");
                assert_eq!(refresh_token, "rt-fresh");
                assert_eq!(expires_at_ms, 1_900_000_000_000);
                assert!(last_refresh_ms.is_some(), "login stamps last_refresh_ms");
            }
            other => panic!("unexpected credential {other:?}"),
        }
    }

    #[test]
    fn account_from_exchange_rejects_missing_id_token_or_account_id() {
        // Missing id_token entirely.
        let exchange = CodexCodeExchange {
            tokens: OAuthTokens {
                access_token: "at".into(),
                refresh_token: Some("rt".into()),
                expires_at_ms: 0,
            },
            id_token: None,
        };
        assert!(matches!(
            account_from_exchange(exchange),
            Err(AuthError::CodexAuth(_))
        ));
        // id_token without the account-id claim.
        let id_token = fake_jwt(&serde_json::json!({"email": "x@y.z"}));
        let exchange = CodexCodeExchange {
            tokens: OAuthTokens {
                access_token: "at".into(),
                refresh_token: Some("rt".into()),
                expires_at_ms: 0,
            },
            id_token: Some(id_token),
        };
        assert!(matches!(
            account_from_exchange(exchange),
            Err(AuthError::CodexAuth(_))
        ));
    }

    /// The code exchange must POST form-urlencoded with the PKCE verifier and
    /// no client_secret, and surface the id_token for account construction.
    #[tokio::test]
    async fn exchange_codex_code_posts_form_and_returns_id_token() {
        use std::sync::{Arc, Mutex};
        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_in_handler = Arc::clone(&seen);
        let id_token = fake_jwt(&serde_json::json!({
            "email": "z@example.com",
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct-xyz"}
        }));
        let access = fake_jwt(&serde_json::json!({"exp": 1_950_000_000}));
        let body = format!(
            r#"{{"id_token":"{id_token}","access_token":"{access}","refresh_token":"rt-x"}}"#
        );
        let app = axum::Router::new().route(
            "/oauth/token",
            axum::routing::post(move |req: axum::extract::Request| {
                let seen = Arc::clone(&seen_in_handler);
                let body = body.clone();
                async move {
                    let content_type = req
                        .headers()
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    let bytes = axum::body::to_bytes(req.into_body(), usize::MAX)
                        .await
                        .expect("body");
                    seen.lock()
                        .expect("lock")
                        .push((content_type, String::from_utf8_lossy(&bytes).into_owned()));
                    ([(http::header::CONTENT_TYPE, "application/json")], body)
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

        let client = reqwest::Client::new();
        let pkce = PkcePair::from_verifier_for_test("v".repeat(43));
        let exchange = exchange_codex_code(
            &client,
            &format!("http://{addr}/oauth/token"),
            "the-auth-code",
            "http://localhost:1455/auth/callback",
            &pkce,
        )
        .await
        .expect("exchange");
        assert_eq!(exchange.tokens.access_token, access);
        assert_eq!(exchange.tokens.refresh_token.as_deref(), Some("rt-x"));
        assert_eq!(exchange.id_token.as_deref(), Some(id_token.as_str()));

        let seen = seen.lock().expect("lock").clone();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].0.starts_with("application/x-www-form-urlencoded"));
        let posted = &seen[0].1;
        assert!(posted.contains("grant_type=authorization_code"));
        assert!(posted.contains("code=the-auth-code"));
        assert!(posted.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(posted.contains(&format!("code_verifier={}", "v".repeat(43))));
        assert!(
            posted.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"),
            "redirect_uri must be form-urlencoded: {posted}"
        );
        assert!(!posted.contains("client_secret"), "no client_secret");
    }

    #[test]
    fn parse_codex_auth_fixture() {
        let id_token = fake_jwt(&serde_json::json!({"email": "codex@example.com"}));
        let access_token = fake_jwt(&serde_json::json!({"exp": 1_750_000_000}));
        let raw = format!(
            r#"{{
              "auth_mode": "chatgpt",
              "OPENAI_API_KEY": null,
              "tokens": {{
                "id_token": "{id_token}",
                "access_token": "{access_token}",
                "refresh_token": "rt-codex-fake",
                "account_id": "acct-uuid-1"
              }},
              "last_refresh": "2026-06-12T00:00:00Z"
            }}"#
        );
        let account = parse_codex_auth(&raw).expect("parse");
        assert_eq!(account.name, "codex@example.com");
        match account.credential {
            AccountCredential::Codex {
                account_id,
                access_token: at,
                refresh_token,
                expires_at_ms,
                last_refresh_ms,
            } => {
                assert_eq!(account_id, "acct-uuid-1");
                assert_eq!(at, access_token);
                assert_eq!(refresh_token, "rt-codex-fake");
                assert_eq!(expires_at_ms, 1_750_000_000_000);
                assert_eq!(last_refresh_ms, None, "import never stamps a refresh");
            }
            other => panic!("unexpected credential {other:?}"),
        }
    }

    #[test]
    fn parse_codex_auth_opaque_tokens_default_name_and_zero_expiry() {
        let raw = r#"{"tokens":{"id_token":"opaque","access_token":"at-x","refresh_token":"rt-x","account_id":"acct-2"}}"#;
        let account = parse_codex_auth(raw).expect("parse");
        assert_eq!(account.name, "codex", "undecodable id_token → default name");
        match account.credential {
            AccountCredential::Codex { expires_at_ms, .. } => assert_eq!(expires_at_ms, 0),
            other => panic!("unexpected credential {other:?}"),
        }
    }

    #[test]
    fn parse_codex_auth_rejects_missing_fields() {
        assert!(parse_codex_auth("{}").is_err());
        assert!(parse_codex_auth(r#"{"tokens":{"access_token":"at"}}"#).is_err());
        assert!(parse_codex_auth("not json").is_err());
    }

    /// Mock token endpoint asserting the refresh request is form-encoded
    /// with the codex client id.
    #[tokio::test]
    async fn refresh_posts_form_encoded_grant() {
        use std::sync::{Arc, Mutex};
        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_in_handler = Arc::clone(&seen);
        let access = fake_jwt(&serde_json::json!({"exp": 1_900_000_000}));
        let body = format!(r#"{{"access_token":"{access}","refresh_token":"rt-new"}}"#);
        let app = axum::Router::new().route(
            "/oauth/token",
            axum::routing::post(move |req: axum::extract::Request| {
                let seen = Arc::clone(&seen_in_handler);
                let body = body.clone();
                async move {
                    let content_type = req
                        .headers()
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    let bytes = axum::body::to_bytes(req.into_body(), usize::MAX)
                        .await
                        .expect("body");
                    seen.lock()
                        .expect("lock")
                        .push((content_type, String::from_utf8_lossy(&bytes).into_owned()));
                    ([(http::header::CONTENT_TYPE, "application/json")], body)
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

        let client = reqwest::Client::new();
        let tokens = refresh_codex_at(
            &client,
            &format!("http://{addr}/oauth/token"),
            "rt-codex-old",
        )
        .await
        .expect("refresh");
        assert_eq!(tokens.access_token, access);
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt-new"));
        assert_eq!(tokens.expires_at_ms, 1_900_000_000_000, "exp from JWT");

        let seen = seen.lock().expect("lock").clone();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].0.starts_with("application/x-www-form-urlencoded"));
        assert!(seen[0].1.contains("grant_type=refresh_token"));
        assert!(seen[0].1.contains("refresh_token=rt-codex-old"));
        assert!(seen[0].1.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
    }

    #[tokio::test]
    async fn refresh_invalid_grant_is_permanent() {
        let app = axum::Router::new().route(
            "/oauth/token",
            axum::routing::post(|| async {
                (
                    http::StatusCode::BAD_REQUEST,
                    r#"{"error":"invalid_grant"}"#,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::new();
        let err = refresh_codex_at(&client, &format!("http://{addr}/oauth/token"), "rt-dead")
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::RefreshPermanent { .. }));
    }
}
