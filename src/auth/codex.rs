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

use super::oauth::OAuthTokens;
use super::AuthError;
use crate::config::{AccountConfig, AccountCredential};

/// OAuth client id the codex CLI uses for refresh-token grants.
pub const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

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
