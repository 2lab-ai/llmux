//! Backend acceptance for the grok usage source (`docs/grok/spec.md` §R3):
//! the xAI CLI billing endpoint feeds the account's WEEKLY allowance into the
//! 7d gauge, through BOTH paths — the startup/periodic usage poller and the
//! explicit `POST /llmux/refresh-usage`.
//!
//! Isolation: every test owns a billing mock (never production — the daemon's
//! `grok.upstream` points at the mock and the billing URL is derived from it),
//! a proxy on port 0 and a tempdir config. No real credential, no real xAI
//! call.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::routing::get;
use axum::Router;
use llmux::config::{self, AccountConfig, AccountCredential, Config};
use llmux::proxy::server::{serve, AppState};
use llmux::scheduler::headers::WindowReading;
use llmux::scheduler::usage::UsageSnapshot;
use llmux::scheduler::{AccountId, AccountPool};

// ---------------------------------------------------------------------------
// Live fixtures (captured 2026-09-17 against cli-chat-proxy.grok.com, VERBATIM)
// ---------------------------------------------------------------------------

const LIVE_BILLING: &str = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-09-15T02:19:18.817992+00:00","end":"2026-09-22T02:19:18.817992+00:00"},"creditUsagePercent":65.0,"onDemandCap":{"val":0},"onDemandUsed":{"val":0},"productUsage":[{"product":"GrokBuild","usagePercent":65.0},{"product":"GrokChat"}],"isUnifiedBillingUser":true,"prepaidBalance":{"val":0},"topUpMethod":"TOP_UP_METHOD_SAVED_PAYMENT_METHOD","billingPeriodStart":"2026-09-15T02:19:18.817992+00:00","billingPeriodEnd":"2026-09-22T02:19:18.817992+00:00"}}"#;

/// The same document at a different utilization, so a test can tell the
/// poller's reading apart from an explicit refresh's.
const QUIET_BILLING: &str = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-09-15T02:19:18.817992+00:00","end":"2026-09-22T02:19:18.817992+00:00"},"creditUsagePercent":7.0,"billingPeriodEnd":"2026-09-22T02:19:18.817992+00:00"}}"#;

/// The live expired-token body (HTTP 401), verbatim.
const EXPIRED_BODY: &str = r#"{"error":"Invalid or expired credentials (auth_kind=bearer, x_xai_token_auth=xai-grok-cli, upstream=PermissionDenied, reason=no auth context)"}"#;

// ---------------------------------------------------------------------------
// Billing mock
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    query: String,
    authorization: Option<String>,
    token_auth: Option<String>,
    user_id: Option<String>,
    client_version: Option<String>,
    accept: Option<String>,
}

#[derive(Default)]
struct MockState {
    seen: Mutex<Vec<Seen>>,
    billing: Mutex<VecDeque<(u16, String)>>,
}

struct BillingMock {
    addr: SocketAddr,
    state: Arc<MockState>,
}

impl BillingMock {
    async fn spawn() -> Self {
        let state = Arc::new(MockState::default());
        let app = Router::new()
            .route("/v1/billing", get(handle_billing))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { addr, state }
    }

    /// The value a daemon would carry in `grok.upstream`: the `/v1` chat base,
    /// from which the billing URL is derived.
    fn grok_upstream(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.addr.port())
    }

    fn push(&self, status: u16, body: &str) {
        self.state
            .billing
            .lock()
            .expect("lock")
            .push_back((status, body.to_string()));
    }

    fn seen(&self) -> Vec<Seen> {
        self.state.seen.lock().expect("lock").clone()
    }
}

async fn handle_billing(
    axum::extract::State(state): axum::extract::State<Arc<MockState>>,
    uri: http::Uri,
    headers: http::HeaderMap,
) -> (http::StatusCode, String) {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    state.seen.lock().expect("lock").push(Seen {
        path: uri.path().to_string(),
        query: uri.query().unwrap_or_default().to_string(),
        authorization: header("authorization"),
        token_auth: header("x-xai-token-auth"),
        user_id: header("x-userid"),
        client_version: header("x-grok-client-version"),
        accept: header("accept"),
    });
    let scripted = state.billing.lock().expect("lock").pop_front();
    let (status, body) = scripted.unwrap_or_else(|| (200, QUIET_BILLING.to_string()));
    (http::StatusCode::from_u16(status).expect("status"), body)
}

// ---------------------------------------------------------------------------
// Proxy harness (mirrors tests/usage_controls.rs)
// ---------------------------------------------------------------------------

const ADMIN_KEY: &str = "lm-grok-admin";

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "llmux-grok-usage-{}-{}",
            std::process::id(),
            ulid::Ulid::new()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn epoch_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

/// Beyond the background refresh window, so no test's mock ever sees a token
/// refresh it did not script.
fn far_future_ms() -> u64 {
    epoch_ms_now() + 24 * 3_600 * 1_000
}

fn grok_account(name: &str) -> AccountConfig {
    AccountConfig {
        name: name.to_string(),
        credential: AccountCredential::Grok {
            subject: format!("sub-{name}"),
            access_token: format!("at-{name}"),
            refresh_token: format!("rt-{name}"),
            expires_at_ms: far_future_ms(),
            token_endpoint: String::new(),
            last_refresh_ms: None,
        },
    }
}

fn apikey_account(name: &str) -> AccountConfig {
    AccountConfig {
        name: name.to_string(),
        credential: AccountCredential::Apikey {
            api_key: format!("sk-ant-{name}"),
        },
    }
}

struct Proxy {
    addr: SocketAddr,
    pool: AccountPool,
    _tmp: TempDir,
}

impl Proxy {
    /// A daemon whose grok upstream is the mock — never the production
    /// `cli-chat-proxy.grok.com` default.
    async fn spawn(mock: &BillingMock, accounts: Vec<AccountConfig>) -> Self {
        let mut config = Config {
            accounts,
            ..Default::default()
        };
        config.grok.upstream = mock.grok_upstream();
        Self::spawn_config(config).await
    }

    async fn spawn_config(mut config: Config) -> Self {
        config.proxy.idle_probe.enabled = false;
        if config.proxy.api_key.is_none() {
            config.proxy.api_key = Some(ADMIN_KEY.into());
        }
        let tmp = TempDir::new();
        let config_path = tmp.path().join("llmux.json");
        config.proxy.port = 0;
        config::save_path(&config_path, &config).expect("seed config");

        let pool = AccountPool::new(&config.accounts);
        let mut state = AppState::new(config, pool.clone(), None, None).expect("app state");
        state.config_path = Some(config_path);
        state.activity_log_path = Some(tmp.path().join("activity.jsonl"));
        state.raw_io_path = Some(tmp.path().join("raw-io.jsonl"));
        state.usage_control_state_path = Some(tmp.path().join("usage-resets.json"));

        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(serve(state, Some(ready_tx)));
        let addr = ready_rx.await.expect("proxy ready");
        Self {
            addr,
            pool,
            _tmp: tmp,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.addr.port(), path)
    }
}

async fn post_admin(
    proxy: &Proxy,
    path: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let response = reqwest::Client::new()
        .post(proxy.url(path))
        .header("x-api-key", ADMIN_KEY)
        .json(&body)
        .send()
        .await
        .expect("proxy reachable");
    let status = response.status().as_u16();
    let text = response.text().await.expect("body");
    (status, parse_json(&text))
}

async fn get_admin(proxy: &Proxy, path: &str) -> (u16, serde_json::Value) {
    let response = reqwest::Client::new()
        .get(proxy.url(path))
        .header("x-api-key", ADMIN_KEY)
        .send()
        .await
        .expect("proxy reachable");
    let status = response.status().as_u16();
    let text = response.text().await.expect("body");
    (status, parse_json(&text))
}

fn parse_json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or(serde_json::Value::Null)
}

/// One account's object out of `GET /llmux/dashboard` — the SAME document the
/// TUI and `llmux status` render, so asserting here proves the 7d gauge
/// reaches the surfaces without any grok special-casing.
async fn dashboard_account(proxy: &Proxy, name: &str) -> serde_json::Value {
    let (status, doc) = get_admin(proxy, "/llmux/dashboard").await;
    assert_eq!(status, 200, "dashboard readable");
    doc["accounts"]
        .as_array()
        .expect("accounts array")
        .iter()
        .find(|a| a["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("account {name} in dashboard"))
}

/// Seed an exhausted cached weekly window, as a grok account looked before
/// this feature existed (or after a quota reset upstream).
fn seed_exhausted(pool: &AccountPool, name: &str) {
    pool.record_usage(
        &AccountId(name.to_string()),
        &UsageSnapshot {
            five_hour: Some(WindowReading {
                utilization: 0.25,
                resets_at: SystemTime::now() + Duration::from_secs(60),
            }),
            seven_day: Some(WindowReading {
                utilization: 1.0,
                resets_at: SystemTime::now() + Duration::from_secs(3_600),
            }),
            scoped: Vec::new(),
        },
        SystemTime::now(),
    );
}

// ---------------------------------------------------------------------------
// Explicit refresh (`POST /llmux/refresh-usage`)
// ---------------------------------------------------------------------------

/// The explicit refresh reads the billing endpoint with the grok-CLI identity
/// set, the WEEKLY period lands on `seven_day` (65 → 0.65) replacing a stale
/// 100% cache, and the header-fed 5h burst gauge is left alone.
#[tokio::test]
async fn refresh_usage_records_the_weekly_billing_window_for_grok() {
    let mock = BillingMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![grok_account("grok:g")]).await;
    seed_exhausted(&proxy.pool, "grok:g");
    mock.push(200, LIVE_BILLING);

    let (status, body) = post_admin(
        &proxy,
        "/llmux/refresh-usage",
        serde_json::json!({"account":"grok:g"}),
    )
    .await;
    assert_eq!(status, 200, "refresh accepted: {body}");
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["results"][0]["provider"], "grok", "{body}");

    let seen = mock.seen().last().cloned().expect("a billing GET");
    assert_eq!(seen.path, "/v1/billing");
    assert_eq!(seen.query, "format=credits");
    assert_eq!(seen.authorization.as_deref(), Some("Bearer at-grok:g"));
    assert_eq!(seen.token_auth.as_deref(), Some("xai-grok-cli"));
    assert_eq!(
        seen.user_id.as_deref(),
        Some("sub-grok:g"),
        "x-userid carries the credential subject"
    );
    assert!(
        seen.client_version.is_some(),
        "the grok client version rides along: {seen:?}"
    );
    assert_eq!(seen.accept.as_deref(), Some("application/json"));

    let account = dashboard_account(&proxy, "grok:g").await;
    let seven = account["seven_day"]["utilization"]
        .as_f64()
        .unwrap_or_else(|| panic!("7d gauge: {account}"));
    assert!(
        (seven - 0.65).abs() < 1e-9,
        "weekly allowance 65% → 0.65, got {seven}: {account}"
    );
    assert_eq!(
        account["five_hour"]["utilization"].as_f64(),
        Some(0.25),
        "the billing read never touches the header-fed 5h gauge: {account}"
    );
}

/// The all-accounts refresh (no `account` in the body) now includes grok
/// alongside codex/oauth; an apikey account stays excluded.
#[tokio::test]
async fn refresh_usage_without_an_account_includes_grok() {
    let mock = BillingMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![grok_account("grok:g"), apikey_account("k")]).await;
    mock.push(200, LIVE_BILLING);

    let (status, body) = post_admin(&proxy, "/llmux/refresh-usage", serde_json::json!({})).await;
    assert_eq!(status, 200, "{body}");
    let results = body["results"].as_array().expect("results").clone();
    assert_eq!(
        results.len(),
        1,
        "only the grok account is refreshable: {body}"
    );
    assert_eq!(results[0]["account"], "grok:g");
    assert_eq!(results[0]["ok"], true, "{body}");
    assert_eq!(results[0]["provider"], "grok");

    let account = dashboard_account(&proxy, "grok:g").await;
    assert!(
        (account["seven_day"]["utilization"].as_f64().expect("7d") - 0.65).abs() < 1e-9,
        "{account}"
    );
}

/// A 401 (the live expired-token answer) is a sanitized 502 that RETAINS the
/// previous observation: a failed read never clears a window and never
/// publishes the upstream body.
#[tokio::test]
async fn expired_token_refresh_is_sanitized_and_keeps_the_previous_window() {
    let mock = BillingMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![grok_account("grok:g")]).await;
    mock.push(200, LIVE_BILLING);
    let (status, body) = post_admin(
        &proxy,
        "/llmux/refresh-usage",
        serde_json::json!({"account":"grok:g"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    mock.push(401, EXPIRED_BODY);
    let (status, body) = post_admin(
        &proxy,
        "/llmux/refresh-usage",
        serde_json::json!({"account":"grok:g"}),
    )
    .await;
    // Same envelope as every other provider's failed read (codex parity): the
    // request itself succeeded, the per-account result carries the failure.
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ok"], false, "{body}");
    let message = body["results"][0]["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("HTTP 401"),
        "sanitized status phrase, got {message}"
    );
    assert!(
        !message.contains("auth_kind") && !message.contains("at-grok:g"),
        "neither the upstream body nor the token may leak: {message}"
    );

    let account = dashboard_account(&proxy, "grok:g").await;
    assert!(
        (account["seven_day"]["utilization"].as_f64().expect("7d") - 0.65).abs() < 1e-9,
        "a failed read retains the last good reading: {account}"
    );
}

/// A grok upstream the billing URL cannot be derived from is REFUSED —
/// llmux never silently falls back to the production chat proxy. The refusal
/// rides the standard per-account failure envelope (codex `wham_base` parity)
/// and no request leaves the process.
#[tokio::test]
async fn underivable_grok_upstream_is_refused_without_upstream_io() {
    let mock = BillingMock::spawn().await;
    let mut config = Config {
        accounts: vec![grok_account("grok:g")],
        ..Default::default()
    };
    // Same host as the mock, but not the `/v1` base the billing URL needs.
    config.grok.upstream = format!("http://127.0.0.1:{}/v2", mock.addr.port());
    let proxy = Proxy::spawn_config(config).await;

    let (status, body) = post_admin(
        &proxy,
        "/llmux/refresh-usage",
        serde_json::json!({"account":"grok:g"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ok"], false, "refused, not defaulted: {body}");
    let message = body["results"][0]["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("path does not end in /v1"),
        "the refusal names the reason, got {message}"
    );
    assert!(
        mock.seen().is_empty(),
        "no request may reach any upstream: {:?}",
        mock.seen()
    );
}

// ---------------------------------------------------------------------------
// Periodic poller
// ---------------------------------------------------------------------------

/// Grok accounts were never scheduled by the usage poller before this change
/// (`poll_account` only handled `Oauth`), so an idle grok account's 7d gauge
/// stayed empty. The daemon's startup priming pass now polls it — no explicit
/// refresh, no inference request.
#[tokio::test]
async fn the_usage_poller_primes_grok_accounts_from_billing() {
    let mock = BillingMock::spawn().await;
    // Unscripted → the mock answers QUIET_BILLING (7%), so this reading can
    // only have come from the poller.
    let proxy = Proxy::spawn(&mock, vec![grok_account("grok:g")]).await;

    let seen = mock.seen();
    assert!(
        !seen.is_empty(),
        "the poller must have read billing during priming"
    );
    assert_eq!(seen[0].path, "/v1/billing");
    assert_eq!(seen[0].authorization.as_deref(), Some("Bearer at-grok:g"));

    let account = dashboard_account(&proxy, "grok:g").await;
    let seven = account["seven_day"]["utilization"]
        .as_f64()
        .unwrap_or_else(|| panic!("7d gauge: {account}"));
    assert!(
        (seven - 0.07).abs() < 1e-9,
        "the poller's weekly reading (7%) is on the gauge, got {seven}: {account}"
    );
    assert!(
        account["five_hour"]["utilization"].is_null(),
        "billing carries no burst window — the 5h gauge stays cold: {account}"
    );
}
