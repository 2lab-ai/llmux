//! OpenRouter login: OAuth PKCE against `openrouter.ai/auth` with a localhost
//! callback, then a code→**API key** exchange.
//!
//! Unlike the Claude/Codex flows this is NOT a token flow: the exchange
//! returns a long-lived `sk-or-v1-…` API key with no refresh token and no
//! expiry, so there is nothing here to refresh (docs/openrouter/spec.md §R5).
//! Two more deviations from the shared PKCE machinery, both live-probed
//! 2026-08-21:
//! - the callback carries only `code` — no `state` — hence
//!   `run_callback_server(..., None)`;
//! - errors come back in the OpenAI-ish `{"error":{"message","code"}}` shape,
//!   not the Anthropic envelope, so the message is unwrapped by hand.
//!
//! The minted key is a credential: it is never printed, logged or traced.

use axum::http::header;
use serde_json::Value;

use super::oauth::{self, PkcePair};
use super::AuthError;

/// Browser authorization endpoint (docs use-cases/oauth-pkce).
pub const AUTHORIZE_URL: &str = "https://openrouter.ai/auth";

/// Code→API-key exchange endpoint.
pub const KEYS_URL: &str = "https://openrouter.ai/api/v1/auth/keys";

/// Key introspection (Bearer) — used only to name the account.
pub const KEY_INFO_URL: &str = "https://openrouter.ai/api/v1/key";

/// Local callback path. OpenRouter accepts "any port" on localhost, so the
/// listener binds port 0 and the path is ours to pick.
const CALLBACK_PATH: &str = "/callback";

/// Build the browser authorize URL: `callback_url` + PKCE S256 challenge.
/// No `client_id`, no `scope`, no `state` — OpenRouter's authorize takes
/// exactly these three parameters.
pub fn authorize_url(challenge: &str, redirect_uri: &str) -> String {
    let params: [(&str, &str); 3] = [
        ("callback_url", redirect_uri),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
    ];
    let mut url = String::from(AUTHORIZE_URL);
    for (i, (key, value)) in params.iter().enumerate() {
        url.push(if i == 0 { '?' } else { '&' });
        url.push_str(key);
        url.push('=');
        url.push_str(&oauth::urlencode(value));
    }
    url
}

/// Exchange an authorization code for the API key.
///
/// `keys_url` is injectable so tests can hit a local mock. Codes are
/// single-use and valid 10 minutes; a spent or wrong code answers 400 with
/// `{"error":{"message":"Invalid code","code":400}}`, whose `message` is what
/// [`AuthError::TokenEndpoint`] carries.
pub async fn exchange_code(
    client: &reqwest::Client,
    keys_url: &str,
    code: &str,
    verifier: &str,
) -> Result<String, AuthError> {
    let body = serde_json::json!({
        "code": code,
        "code_verifier": verifier,
        "code_challenge_method": "S256",
    });

    let response = client
        .post(keys_url)
        .header(header::ACCEPT, "application/json")
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let raw = response.text().await.unwrap_or_default();
        return Err(AuthError::TokenEndpoint {
            status,
            body: error_message(&raw),
        });
    }

    let text = response.text().await?;
    let parsed: Value = serde_json::from_str(&text)?;
    parsed
        .get("key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        // Deliberately does not echo the body: a 200 from this endpoint may
        // carry key material in a shape we failed to recognize.
        .ok_or_else(|| AuthError::Aborted("openrouter auth response missing \"key\"".into()))
}

/// Unwrap OpenRouter's `{"error":{"message":…}}` envelope to its message,
/// falling back to the raw body when the shape is anything else.
fn error_message(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| raw.trim().to_string())
}

/// Best-effort key label from `GET /api/v1/key`, used only to name the
/// account (`or:<label>`).
///
/// The success schema is not contractually pinned, so this parses
/// defensively — `data.label`, then a top-level `label` — and degrades to
/// `None` on ANY failure (transport, non-2xx, unparseable, absent field).
/// Naming is cosmetic; it must never cost the user a freshly minted key.
pub async fn fetch_key_label(
    client: &reqwest::Client,
    key_info_url: &str,
    api_key: &str,
) -> Option<String> {
    let response = client
        .get(key_info_url)
        .bearer_auth(api_key)
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let text = response.text().await.ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    label_from_key_info(&parsed)
}

/// Pure half of [`fetch_key_label`] — unit-testable without a socket.
fn label_from_key_info(value: &Value) -> Option<String> {
    value
        .get("data")
        .and_then(|data| data.get("label"))
        .or_else(|| value.get("label"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string)
}

/// Full interactive login: PKCE pair → ephemeral localhost callback → browser
/// → state-less callback → code exchange. Returns the `sk-or-v1-…` key, which
/// the caller persists (and never prints).
pub async fn login_interactive(client: &reqwest::Client) -> Result<String, AuthError> {
    let pkce = PkcePair::generate();

    // "Any port" is accepted, so bind an ephemeral one (as the Claude flow
    // does) rather than fighting for a fixed port.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{port}{CALLBACK_PATH}");

    let url = authorize_url(&pkce.challenge, &redirect_uri);
    eprintln!("Opening browser for OpenRouter authentication...");
    eprintln!("If it doesn't open, visit:\n  {url}\n");
    oauth::open_browser(&url);

    // `None`: OpenRouter's callback has no `state` parameter to echo.
    let code = oauth::run_callback_server(listener, CALLBACK_PATH, None).await?;

    exchange_code(client, KEYS_URL, &code, &pkce.verifier).await
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::Router;

    use super::*;

    #[test]
    fn authorize_url_encodes_callback_and_challenge() {
        let pkce = PkcePair::from_verifier_for_test("test-verifier".to_string());
        let url = authorize_url(&pkce.challenge, "http://localhost:7777/callback");
        assert!(url.starts_with("https://openrouter.ai/auth?callback_url="));
        assert!(url.contains("callback_url=http%3A%2F%2Flocalhost%3A7777%2Fcallback"));
        assert!(url.contains(&format!("code_challenge={}", pkce.challenge)));
        assert!(url.contains("code_challenge_method=S256"));
        // No CSRF state, no client_id, no scope on this provider.
        assert!(!url.contains("state="));
        assert!(!url.contains("client_id="));
    }

    #[test]
    fn key_info_label_parsed_defensively() {
        let nested = serde_json::json!({"data": {"label": "sk-or-v1-abc", "usage": 0}});
        assert_eq!(
            label_from_key_info(&nested).as_deref(),
            Some("sk-or-v1-abc")
        );

        let flat = serde_json::json!({"label": "top-level"});
        assert_eq!(label_from_key_info(&flat).as_deref(), Some("top-level"));

        // Absent / empty / wrong-typed all degrade to no label.
        assert_eq!(label_from_key_info(&serde_json::json!({})), None);
        assert_eq!(
            label_from_key_info(&serde_json::json!({"data": {"label": "  "}})),
            None
        );
        assert_eq!(
            label_from_key_info(&serde_json::json!({"data": {"label": 7}})),
            None
        );
    }

    #[test]
    fn error_message_unwraps_openrouter_envelope() {
        assert_eq!(
            error_message(r#"{"error":{"message":"Invalid code","code":400}}"#),
            "Invalid code"
        );
        // Not the expected shape → raw body survives.
        assert_eq!(error_message("  upstream exploded  "), "upstream exploded");
        assert_eq!(error_message(r#"{"error":{}}"#), r#"{"error":{}}"#);
    }

    /// Mock `/api/v1/auth/keys`: serves `responses` in order (last repeats),
    /// recording the request bodies it saw.
    async fn spawn_keys_mock(
        responses: Vec<(StatusCode, String)>,
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    ) -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_in_handler = Arc::clone(&hits);
        let responses = Arc::new(responses);
        let app = Router::new().route(
            "/api/v1/auth/keys",
            post(move |body: String| {
                let hits = Arc::clone(&hits_in_handler);
                let responses = Arc::clone(&responses);
                let seen = Arc::clone(&seen);
                async move {
                    let i = hits.fetch_add(1, Ordering::SeqCst);
                    seen.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(body);
                    responses[i.min(responses.len() - 1)].clone()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/api/v1/auth/keys"), hits)
    }

    #[tokio::test]
    async fn exchange_code_posts_pkce_body_and_returns_key() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (url, hits) = spawn_keys_mock(
            vec![(StatusCode::OK, r#"{"key":"sk-or-v1-secret"}"#.into())],
            Arc::clone(&seen),
        )
        .await;
        let client = reqwest::Client::new();
        let key = exchange_code(&client, &url, "the-code", "the-verifier")
            .await
            .unwrap();
        assert_eq!(key, "sk-or-v1-secret");
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        let body = seen.lock().unwrap()[0].clone();
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["code"], "the-code");
        assert_eq!(body["code_verifier"], "the-verifier");
        assert_eq!(body["code_challenge_method"], "S256");
    }

    #[tokio::test]
    async fn exchange_code_surfaces_error_message() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (url, _) = spawn_keys_mock(
            vec![(
                StatusCode::BAD_REQUEST,
                r#"{"error":{"message":"Invalid code","code":400}}"#.into(),
            )],
            seen,
        )
        .await;
        let client = reqwest::Client::new();
        let err = exchange_code(&client, &url, "stale", "v")
            .await
            .unwrap_err();
        match err {
            AuthError::TokenEndpoint { status, body } => {
                assert_eq!(status, StatusCode::BAD_REQUEST);
                assert_eq!(body, "Invalid code");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn exchange_code_missing_key_field_aborts() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (url, _) = spawn_keys_mock(
            vec![(StatusCode::OK, r#"{"ok":true,"key":""}"#.into())],
            seen,
        )
        .await;
        let client = reqwest::Client::new();
        let err = exchange_code(&client, &url, "c", "v").await.unwrap_err();
        assert!(
            matches!(&err, AuthError::Aborted(message) if message.contains("missing \"key\"")),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn fetch_key_label_degrades_on_failure() {
        // Nothing listening: transport error → no label, no panic.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/v1/key", listener.local_addr().unwrap());
        drop(listener);
        let client = reqwest::Client::new();
        assert_eq!(fetch_key_label(&client, &url, "sk-or-v1-x").await, None);
    }
}
