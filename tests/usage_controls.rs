//! Backend acceptance for the Codex usage controls (`.prd/16-codex-usage-controls.md`,
//! traces S1/S2/S3): explicit usage refresh, reset-credit list/redeem, and the
//! refresh-before-commit manual switch.
//!
//! Isolation: every test owns a WHAM mock (never production — the daemon's
//! `codex.upstream` is pointed at the mock and the base is derived from it),
//! a proxy on port 0, and a tempdir config. NO real credential, NO real
//! redemption: the consume endpoint here is a local axum handler.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::routing::{get, post};
use axum::Router;
use llmux::config::{self, AccountConfig, AccountCredential, Config};
use llmux::proxy::server::{serve, AppState};
use llmux::scheduler::headers::WindowReading;
use llmux::scheduler::usage::UsageSnapshot;
use llmux::scheduler::{AccountId, AccountPool};

// ---------------------------------------------------------------------------
// Live-SHAPED fixtures (`.prd/16-codex-usage-controls.md` §Research, captured
// 2026-09-11). `LIVE_USAGE` is the capture verbatim: primary_window is WEEKLY
// (`limit_window_seconds: 604800`) with secondary null — position does not
// determine the window kind, duration does.
//
// `LIVE_CREDITS` is the capture PLUS a synthetic `id`: the capture's
// `id`/`profile_*` fields were removed by the sanitizer before it was saved,
// so their absence there is not evidence about the upstream schema (the codex
// client's own type requires `id`). The id here is invented so the tests can
// address one specific credit.
// ---------------------------------------------------------------------------

const LIVE_USAGE: &str = r#"{
  "rate_limit": {
    "allowed": true,
    "limit_reached": false,
    "primary_window": {
      "used_percent": 43,
      "limit_window_seconds": 604800,
      "reset_after_seconds": 349683,
      "reset_at": 1789442251
    },
    "secondary_window": null
  },
  "rate_limit_reset_credits": { "available_count": 3, "applicable_available_count": 0 }
}"#;

const LIVE_CREDITS: &str = r#"{
  "available_count": 3,
  "credits": [
    { "id": "credit-1", "reset_type": "codex_rate_limits", "status": "available",
      "granted_at": "2026-08-22T00:00:34.470976Z", "expires_at": "2026-09-21T00:00:34.470976Z",
      "title": "Full reset", "description": "You've been granted one free rate limit reset." }
  ]
}"#;

// ---------------------------------------------------------------------------
// WHAM mock
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: Option<String>,
    account_id: Option<String>,
    body: String,
}

/// One scripted reply: status, body, and an optional delay that makes a slow
/// upstream reproducible (the concurrency tests need guaranteed overlap).
type Scripted = (u16, String, u64);

/// A one-shot gate on the next usage GET: the handler announces arrival on the
/// sender and withholds its response until the receiver fires. Lets a test
/// interleave with an in-flight request EXACTLY, with no sleeps.
type UsageGate = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

#[derive(Default)]
struct MockState {
    seen: Mutex<Vec<Seen>>,
    usage: Mutex<VecDeque<Scripted>>,
    credits: Mutex<VecDeque<Scripted>>,
    consume: Mutex<VecDeque<Scripted>>,
    usage_gate: Mutex<Option<UsageGate>>,
}

struct WhamMock {
    addr: SocketAddr,
    state: Arc<MockState>,
}

impl WhamMock {
    async fn spawn() -> Self {
        let state = Arc::new(MockState::default());
        let app = Router::new()
            .route("/backend-api/wham/usage", get(handle_usage))
            .route(
                "/backend-api/wham/rate-limit-reset-credits",
                get(handle_credits),
            )
            .route(
                "/backend-api/wham/rate-limit-reset-credits/consume",
                post(handle_consume),
            )
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

    /// The value a daemon would carry in `codex.upstream`: the responses base,
    /// whose trailing `/codex` the service strips to reach WHAM.
    fn codex_upstream(&self) -> String {
        format!("http://127.0.0.1:{}/backend-api/codex", self.addr.port())
    }

    fn push_usage(&self, status: u16, body: &str) {
        self.state
            .usage
            .lock()
            .expect("lock")
            .push_back((status, body.to_string(), 0));
    }

    fn push_credits(&self, status: u16, body: &str) {
        self.state
            .credits
            .lock()
            .expect("lock")
            .push_back((status, body.to_string(), 0));
    }

    /// Arm the one-shot gate on the NEXT usage GET. Returns
    /// `(arrived, release)`: awaiting `arrived` resolves the instant the mock
    /// has the request in hand — which means the daemon has already captured
    /// the credential it is reading with — and the response is withheld until
    /// `release.send(())`. That is the deterministic seam an in-flight
    /// interleaving test needs; no sleeps, no timing assumptions.
    fn gate_usage(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *self.state.usage_gate.lock().expect("lock") = Some((arrived_tx, release_rx));
        (arrived_rx, release_tx)
    }

    fn push_usage_delayed(&self, status: u16, body: &str, delay_ms: u64) {
        self.state
            .usage
            .lock()
            .expect("lock")
            .push_back((status, body.to_string(), delay_ms));
    }

    fn push_credits_delayed(&self, status: u16, body: &str, delay_ms: u64) {
        self.state
            .credits
            .lock()
            .expect("lock")
            .push_back((status, body.to_string(), delay_ms));
    }

    fn push_consume(&self, status: u16, body: &str) {
        self.state
            .consume
            .lock()
            .expect("lock")
            .push_back((status, body.to_string(), 0));
    }

    fn seen(&self) -> Vec<Seen> {
        self.state.seen.lock().expect("lock").clone()
    }

    fn seen_paths(&self) -> Vec<String> {
        self.seen().into_iter().map(|s| s.path).collect()
    }
}

fn record(state: &MockState, method: &str, path: &str, headers: &http::HeaderMap, body: String) {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    state.seen.lock().expect("lock").push(Seen {
        method: method.to_string(),
        path: path.to_string(),
        authorization: header("authorization"),
        account_id: header("chatgpt-account-id"),
        body,
    });
}

async fn next(queue: &Mutex<VecDeque<Scripted>>, default: &str) -> (http::StatusCode, String) {
    let scripted = queue.lock().expect("lock").pop_front();
    let (status, body, delay_ms) = scripted.unwrap_or_else(|| (200, default.to_string(), 0));
    if delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }
    (http::StatusCode::from_u16(status).expect("status"), body)
}

async fn handle_usage(
    axum::extract::State(state): axum::extract::State<Arc<MockState>>,
    headers: http::HeaderMap,
) -> (http::StatusCode, String) {
    record(&state, "GET", "/wham/usage", &headers, String::new());
    // Hold the response open while the test rearranges the world behind the
    // daemon's back. The guard is released before the await.
    let gate = state.usage_gate.lock().expect("lock").take();
    if let Some((arrived, release)) = gate {
        let _ = arrived.send(());
        let _ = release.await;
    }
    next(&state.usage, LIVE_USAGE).await
}

async fn handle_credits(
    axum::extract::State(state): axum::extract::State<Arc<MockState>>,
    headers: http::HeaderMap,
) -> (http::StatusCode, String) {
    record(
        &state,
        "GET",
        "/wham/rate-limit-reset-credits",
        &headers,
        String::new(),
    );
    next(&state.credits, LIVE_CREDITS).await
}

async fn handle_consume(
    axum::extract::State(state): axum::extract::State<Arc<MockState>>,
    headers: http::HeaderMap,
    body: String,
) -> (http::StatusCode, String) {
    record(
        &state,
        "POST",
        "/wham/rate-limit-reset-credits/consume",
        &headers,
        body,
    );
    next(&state.consume, r#"{"code":"reset","windows_reset":2}"#).await
}

// ---------------------------------------------------------------------------
// Proxy harness (mirrors tests/e2e.rs: tempdir config, port 0, admin key)
// ---------------------------------------------------------------------------

const ADMIN_KEY: &str = "lm-usage-admin";

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "llmux-usage-{}-{}",
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

fn codex_account(name: &str) -> AccountConfig {
    AccountConfig {
        name: name.to_string(),
        credential: AccountCredential::Codex {
            account_id: format!("acct-{name}"),
            access_token: format!("at-{name}"),
            refresh_token: format!("rt-{name}"),
            expires_at_ms: far_future_ms(),
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
    async fn spawn(mock: &WhamMock, accounts: Vec<AccountConfig>) -> Self {
        Self::spawn_config(config_for(mock, accounts)).await
    }

    async fn spawn_config(config: Config) -> Self {
        Self::spawn_with_receipts(config, None).await
    }

    /// `receipts = Some(path)` reuses another proxy's pending-receipt file,
    /// which is how a daemon RESTART is modelled.
    async fn spawn_with_receipts(mut config: Config, receipts: Option<PathBuf>) -> Self {
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
        // Pending redemption receipts land in the tempdir, NEVER in the user's
        // real ~/.local/state/llmux — same isolation rule as the logs above.
        state.usage_control_state_path =
            Some(receipts.unwrap_or_else(|| tmp.path().join("usage-resets.json")));

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

/// A daemon config whose codex endpoints point at the mock — never at the
/// production `chatgpt.com` default.
fn config_for(mock: &WhamMock, accounts: Vec<AccountConfig>) -> Config {
    let mut config = Config {
        accounts,
        ..Default::default()
    };
    config.codex.upstream = mock.codex_upstream();
    config.codex.token_url = format!("http://127.0.0.1:{}/token", mock.addr.port());
    config
}

async fn post_admin(
    proxy: &Proxy,
    path: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    post_admin_owned(proxy.url(path), body).await
}

/// [`post_admin`] over an owned URL, so two calls can be driven concurrently.
async fn post_admin_owned(url: String, body: serde_json::Value) -> (u16, serde_json::Value) {
    let response = reqwest::Client::new()
        .post(url)
        .header("x-api-key", ADMIN_KEY)
        .json(&body)
        .send()
        .await
        .expect("proxy reachable");
    let status = response.status().as_u16();
    let text = response.text().await.expect("body");
    (status, parse_json(&text))
}

/// Every redemption POST the mock has seen.
fn posts(mock: &WhamMock) -> Vec<Seen> {
    mock.seen()
        .into_iter()
        .filter(|s| s.method == "POST")
        .collect()
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

/// One account's object out of `GET /llmux/dashboard`.
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

/// Seed an exhausted cached weekly window, as an externally-reset account
/// looks to llmux before a refresh.
fn seed_exhausted(pool: &AccountPool, name: &str) {
    pool.record_usage(
        &AccountId(name.to_string()),
        &UsageSnapshot {
            five_hour: None,
            seven_day: Some(WindowReading {
                utilization: 1.0,
                resets_at: SystemTime::now() + Duration::from_secs(3_600),
            }),
            scoped: Vec::new(),
        },
        SystemTime::now(),
    );
}

fn seven_day_utilization(account: &serde_json::Value) -> Option<f64> {
    account["seven_day"]["utilization"].as_f64()
}

// ---------------------------------------------------------------------------
// S1 — refresh
// ---------------------------------------------------------------------------

/// Trace S1.1/S1.2: the refresh endpoint is control plane — a non-admin
/// (keyless loopback) caller is refused BEFORE any upstream IO.
#[tokio::test]
async fn refresh_usage_requires_admin_and_does_no_upstream_io() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;

    let response = reqwest::Client::new()
        .post(proxy.url("/llmux/refresh-usage"))
        .json(&serde_json::json!({ "account": "cx" }))
        .send()
        .await
        .expect("proxy reachable");
    assert_eq!(response.status(), 403, "admin credential required");
    assert!(
        mock.seen().is_empty(),
        "auth refusal must precede upstream IO, saw {:?}",
        mock.seen_paths()
    );
}

/// Trace S1.3/S1.7: the selected account's own identity reaches WHAM (Bearer +
/// ChatGPT-Account-Id), the live-shaped WEEKLY primary window lands on
/// seven_day (duration 604800, not position), 43 normalizes to 0.43, and the
/// stale 100% cache is replaced.
#[tokio::test]
async fn refresh_replaces_exhausted_window_with_live_weekly_primary() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx"), codex_account("cy")]).await;
    seed_exhausted(&proxy.pool, "cx");

    let (status, body) = post_admin(
        &proxy,
        "/llmux/refresh-usage",
        serde_json::json!({"account":"cx"}),
    )
    .await;
    assert_eq!(status, 200, "refresh accepted: {body}");
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["results"][0]["account"], "cx");
    assert_eq!(body["results"][0]["ok"], true, "{body}");

    let seen = mock.seen();
    assert_eq!(seen.len(), 1, "one GET for one account: {:?}", seen);
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].path, "/wham/usage");
    assert_eq!(
        seen[0].authorization.as_deref(),
        Some("Bearer at-cx"),
        "selected account's token"
    );
    assert_eq!(
        seen[0].account_id.as_deref(),
        Some("acct-cx"),
        "selected account's ChatGPT-Account-Id"
    );

    let cx = dashboard_account(&proxy, "cx").await;
    let seven = seven_day_utilization(&cx).expect("weekly window recorded");
    assert!(
        (seven - 0.43).abs() < 1e-9,
        "weekly primary (604800s) lands on seven_day at 0.43, got {seven}"
    );
    assert!(
        cx["five_hour"].is_null(),
        "a weekly window must not be filed as five_hour: {cx}"
    );
    assert_eq!(
        cx["usage_control"]["available_resets"], 3,
        "available_count surfaced: {cx}"
    );
    assert_eq!(
        cx["usage_control"]["applicable_resets"], 0,
        "applicable_available_count is distinct from available_count: {cx}"
    );
    assert!(
        cx["usage_control"]["last_refresh_ms"].as_u64().is_some(),
        "successful refresh timestamp: {cx}"
    );

    let cy = dashboard_account(&proxy, "cy").await;
    assert!(
        cy["usage_control"].is_null(),
        "untargeted account is untouched: {cy}"
    );
}

/// Trace S1.4/S1.5: a failed refresh keeps the prior observations and prior
/// counts, reports a sanitized error, and never fabricates a zero window.
#[tokio::test]
async fn failed_refresh_retains_previous_state_and_counts() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;

    // First refresh succeeds → counts + window observed.
    let (_, ok) = post_admin(
        &proxy,
        "/llmux/refresh-usage",
        serde_json::json!({"account":"cx"}),
    )
    .await;
    assert_eq!(ok["results"][0]["ok"], true, "{ok}");

    // Second fails with an HTML error body (must NOT parse into empty state).
    mock.push_usage(502, "<html><body>bad gateway</body></html>");
    let (status, body) = post_admin(
        &proxy,
        "/llmux/refresh-usage",
        serde_json::json!({"account":"cx"}),
    )
    .await;
    assert_eq!(status, 200, "per-account failure is reported, not thrown");
    assert_eq!(
        body["ok"], false,
        "partial failure is not silently green: {body}"
    );
    assert_eq!(body["results"][0]["ok"], false, "{body}");
    let error = body["results"][0]["error"].as_str().expect("error text");
    assert!(!error.contains("at-cx"), "no credential in error: {error}");
    assert!(
        !error.contains("bad gateway"),
        "no raw body in error: {error}"
    );

    let cx = dashboard_account(&proxy, "cx").await;
    let seven = seven_day_utilization(&cx).expect("prior window retained");
    assert!((seven - 0.43).abs() < 1e-9, "prior observation kept: {cx}");
    assert_eq!(
        cx["usage_control"]["available_resets"], 3,
        "prior counts kept on failure: {cx}"
    );
    assert!(
        cx["usage_control"]["last_error"].as_str().is_some(),
        "sanitized error surfaced: {cx}"
    );
}

/// Trace S1.5: a window that is PRESENT but unreadable never becomes a valid
/// zero. It fails the read (so nothing claims to be fresh), the previously
/// seen window survives, and an absent reset-credit summary stays UNKNOWN
/// rather than 0.
#[tokio::test]
async fn malformed_usage_body_does_not_fabricate_zeros() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;
    seed_exhausted(&proxy.pool, "cx");
    mock.push_usage(
        200,
        r#"{"rate_limit":{"primary_window":{"used_percent":"high","limit_window_seconds":604800}}}"#,
    );

    let (_, body) = post_admin(
        &proxy,
        "/llmux/refresh-usage",
        serde_json::json!({"account":"cx"}),
    )
    .await;
    assert_eq!(
        body["results"][0]["ok"], false,
        "an unreadable window is a failed read, not an empty success: {body}"
    );

    let cx = dashboard_account(&proxy, "cx").await;
    let seven = seven_day_utilization(&cx).expect("prior window retained");
    assert!(
        (seven - 1.0).abs() < 1e-9,
        "the previous observation survives, nothing is zeroed: {cx}"
    );
    assert!(
        cx["five_hour"].is_null(),
        "and no window is fabricated: {cx}"
    );
    assert!(
        cx["usage_control"]["available_resets"].is_null(),
        "absent reset count is unknown, not zero: {cx}"
    );
    assert!(
        cx["usage_control"]["last_refresh_ms"].is_null(),
        "an unusable body cannot advance the successful-refresh timestamp: {cx}"
    );
}

/// Trace S1.5: a body that is not a usage document — `{}`, an unrelated JSON
/// object, or windows whose fields cannot be read — is a FAILURE, not an empty
/// success. It must not advance the refresh timestamp or claim fresh quota. A
/// document with legitimately NULL windows is still a success (the live shape
/// has `secondary_window: null`).
#[tokio::test]
async fn structurally_invalid_usage_body_is_a_failure_not_an_empty_success() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;

    for body in [
        "{}",
        r#"{"rate_limit":{}}"#,
        r#"{"detail":"something else entirely"}"#,
        r#"{"rate_limit":"nope"}"#,
    ] {
        mock.push_usage(200, body);
        let (status, response) = post_admin(
            &proxy,
            "/llmux/refresh-usage",
            serde_json::json!({"account":"cx"}),
        )
        .await;
        assert_eq!(status, 200, "{body}: {response}");
        assert_eq!(
            response["results"][0]["ok"], false,
            "{body} is not a usable usage document: {response}"
        );
        let cx = dashboard_account(&proxy, "cx").await;
        assert!(
            cx["usage_control"]["last_refresh_ms"].is_null(),
            "{body} must not count as a successful refresh: {cx}"
        );
    }

    // …but explicit nulls are a VALID document with absent windows.
    mock.push_usage(
        200,
        r#"{"rate_limit":{"primary_window":null,"secondary_window":null}}"#,
    );
    let (_, response) = post_admin(
        &proxy,
        "/llmux/refresh-usage",
        serde_json::json!({"account":"cx"}),
    )
    .await;
    assert_eq!(
        response["results"][0]["ok"], true,
        "null windows are legitimately absent, not malformed: {response}"
    );
    let cx = dashboard_account(&proxy, "cx").await;
    assert!(
        cx["five_hour"].is_null() && cx["seven_day"].is_null(),
        "…and nothing is fabricated for them: {cx}"
    );
    assert!(
        cx["usage_control"]["last_refresh_ms"].as_u64().is_some(),
        "a valid read does advance the timestamp: {cx}"
    );
}

/// Trace S1.2: unknown account 404; an account whose provider has no usage
/// control (api key) is 422 — neither spends an inference request.
#[tokio::test]
async fn unknown_account_is_404_and_unsupported_provider_is_422() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx"), apikey_account("k")]).await;

    let (status, _) = post_admin(
        &proxy,
        "/llmux/refresh-usage",
        serde_json::json!({"account":"nope"}),
    )
    .await;
    assert_eq!(status, 404, "unknown account");
    let (status, _) = post_admin(
        &proxy,
        "/llmux/refresh-usage",
        serde_json::json!({"account":"k"}),
    )
    .await;
    assert_eq!(status, 422, "unsupported provider");
    assert!(
        mock.seen().is_empty(),
        "no upstream IO for either: {:?}",
        mock.seen_paths()
    );
}

// ---------------------------------------------------------------------------
// S2 — reset list and consume
// ---------------------------------------------------------------------------

/// Trace S2.1/S2.6: the entitlement list is a pure read (no POST) carrying the
/// upstream details.
#[tokio::test]
async fn reset_credit_list_reads_entitlements_without_consuming() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;

    let (status, body) = get_admin(&proxy, "/llmux/reset-credits?account=cx").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["account"], "cx");
    assert_eq!(body["available_count"], 3, "{body}");
    assert_eq!(body["credits"][0]["id"], "credit-1", "{body}");
    assert_eq!(body["credits"][0]["status"], "available", "{body}");

    let seen = mock.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].method, "GET", "listing never mutates");
    assert_eq!(seen[0].account_id.as_deref(), Some("acct-cx"));
}

/// Trace S2.2/S2.3: the redemption is preceded by a FRESH inventory read, the
/// body carries the caller's idempotency key verbatim with the chosen credit
/// id, and `credit_id` is omitted entirely (never null) when the upstream list
/// carries no id.
#[tokio::test]
async fn consume_reads_inventory_then_sends_the_clients_request_id() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;

    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["outcome"], "reset", "{body}");
    assert_eq!(body["request_id"], "rid-1", "{body}");
    assert_eq!(body["windows_reset"], 2, "{body}");

    let first = posts(&mock);
    assert_eq!(first.len(), 1, "exactly one redemption: {first:?}");
    let sent: serde_json::Value = serde_json::from_str(&first[0].body).expect("json body");
    assert_eq!(sent["redeem_request_id"], "rid-1", "{sent}");
    assert_eq!(sent["credit_id"], "credit-1", "the chosen credit: {sent}");
    assert_eq!(first[0].account_id.as_deref(), Some("acct-cx"));
    assert_eq!(first[0].authorization.as_deref(), Some("Bearer at-cx"));
    assert_eq!(
        mock.seen()[0].path,
        "/wham/rate-limit-reset-credits",
        "inventory is read BEFORE the irreversible POST: {:?}",
        mock.seen_paths()
    );

    // A list whose rows carry no id (the live capture's shape) must not
    // serialize `credit_id: null` — upstream then picks the next credit.
    mock.push_credits(
        200,
        r#"{"available_count":2,"credits":[{"reset_type":"codex_rate_limits","status":"available"}]}"#,
    );
    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-2","confirm":true}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let sent: serde_json::Value = serde_json::from_str(&posts(&mock)[1].body).expect("json body");
    assert!(
        sent.get("credit_id").is_none(),
        "optional credit_id is omitted, not null: {sent}"
    );
}

/// Trace S2.3/§Per-account exclusion: an UNCERTAIN outcome keeps the pending
/// receipt — a different key is refused (409, exposing the pending id) and only
/// the original key may retry, bypassing the fresh-inventory gate because the
/// first attempt may already have spent the credit.
#[tokio::test]
async fn uncertain_consume_pins_the_request_id_until_a_terminal_outcome() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;
    mock.push_consume(200, r#"{"code":"teleported"}"#);

    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(status, 502, "uncertain, not success: {body}");
    assert_eq!(body["request_id"], "rid-1", "the id to retry with: {body}");

    // A NEW key is refused while the first outcome is unresolved.
    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-2","confirm":true}),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["pending_request_id"], "rid-1", "{body}");
    assert_eq!(posts(&mock).len(), 1, "no redemption for the new key");

    // The pending id is visible to the operator on the account metadata.
    let cx = dashboard_account(&proxy, "cx").await;
    assert_eq!(cx["usage_control"]["pending_request_id"], "rid-1", "{cx}");

    // The ORIGINAL key retries even though inventory is now unreadable.
    mock.push_credits(500, "nope");
    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(status, 200, "retry allowed: {body}");
    assert_eq!(body["outcome"], "reset", "{body}");
    let posts = posts(&mock);
    assert_eq!(posts.len(), 2, "the retry reached upstream");
    let retried: serde_json::Value = serde_json::from_str(&posts[1].body).expect("json");
    assert_eq!(
        retried["redeem_request_id"], "rid-1",
        "the retry reuses the SAME idempotency key: {retried}"
    );

    // Terminal outcome releases the pending identity.
    let cx = dashboard_account(&proxy, "cx").await;
    assert!(
        cx["usage_control"]["pending_request_id"].is_null(),
        "terminal outcome clears the receipt: {cx}"
    );
}

/// §Crash boundary: the pending receipt is durable — a RESTARTED daemon
/// reading the same state directory still refuses a fresh key.
#[tokio::test]
async fn pending_receipt_survives_a_daemon_restart() {
    let mock = WhamMock::spawn().await;
    let state = TempDir::new();
    let receipts = state.path().join("usage-resets.json");
    mock.push_consume(200, r#"{"code":"teleported"}"#);

    let first = Proxy::spawn_with_receipts(
        config_for(&mock, vec![codex_account("cx")]),
        Some(receipts.clone()),
    )
    .await;
    let (status, _) = post_admin(
        &first,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(status, 502, "uncertain");
    assert!(receipts.exists(), "receipt persisted before the POST");

    let restarted =
        Proxy::spawn_with_receipts(config_for(&mock, vec![codex_account("cx")]), Some(receipts))
            .await;
    let (status, body) = post_admin(
        &restarted,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-fresh","confirm":true}),
    )
    .await;
    assert_eq!(status, 409, "a restart must not spend a fresh key: {body}");
    assert_eq!(body["pending_request_id"], "rid-1", "{body}");
    assert_eq!(
        posts(&mock).len(),
        1,
        "only the first attempt reached upstream"
    );
}

/// §Crash boundary: the receipt registry is SHARED by every account, so
/// concurrent redemptions on DIFFERENT accounts must not overwrite each
/// other's receipt. Every uncertain attempt has to survive into the file a
/// restarted daemon reloads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_pending_receipts_on_different_accounts_all_survive() {
    const ACCOUNTS: usize = 8;
    let mock = WhamMock::spawn().await;
    let state = TempDir::new();
    let receipts = state.path().join("usage-resets.json");
    let accounts: Vec<AccountConfig> = (0..ACCOUNTS)
        .map(|i| codex_account(&format!("cx{i}")))
        .collect();
    for _ in 0..ACCOUNTS {
        // A slow inventory read lines every attempt up on the same instant, so
        // the receipt writes genuinely interleave.
        mock.push_credits_delayed(200, LIVE_CREDITS, 250);
        mock.push_consume(200, r#"{"code":"teleported"}"#);
    }
    let proxy =
        Proxy::spawn_with_receipts(config_for(&mock, accounts), Some(receipts.clone())).await;

    let mut attempts = Vec::new();
    for i in 0..ACCOUNTS {
        attempts.push(tokio::spawn(post_admin_owned(
            proxy.url("/llmux/reset-credits/consume"),
            serde_json::json!({
                "account": format!("cx{i}"),
                "redeem_request_id": format!("rid-{i}"),
                "confirm": true
            }),
        )));
    }
    for attempt in attempts {
        let (status, body) = attempt.await.expect("consume task");
        assert_eq!(status, 502, "every attempt is uncertain: {body}");
    }

    let persisted: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(&receipts).expect("receipt file")).expect("json");
    let mut names: Vec<String> = persisted
        .iter()
        .map(|p| p["account"].as_str().expect("account").to_string())
        .collect();
    names.sort();
    assert_eq!(
        names.len(),
        ACCOUNTS,
        "no account's crash receipt may be lost to a concurrent write: {names:?}"
    );

    // A restarted daemon must refuse a fresh key for EVERY one of them.
    let restarted = Proxy::spawn_with_receipts(
        config_for(
            &mock,
            (0..ACCOUNTS)
                .map(|i| codex_account(&format!("cx{i}")))
                .collect(),
        ),
        Some(receipts),
    )
    .await;
    for i in 0..ACCOUNTS {
        let (status, body) = post_admin(
            &restarted,
            "/llmux/reset-credits/consume",
            serde_json::json!({
                "account": format!("cx{i}"), "redeem_request_id": "rid-fresh", "confirm": true
            }),
        )
        .await;
        assert_eq!(status, 409, "cx{i} still pending after restart: {body}");
        assert_eq!(body["pending_request_id"], format!("rid-{i}"), "{body}");
    }
}

/// Trace S2.4: the post-success INVENTORY re-read is part of the follow-up.
/// Its failure is a stale-read warning, not silence.
#[tokio::test]
async fn post_consume_inventory_failure_is_reported_as_a_warning() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;
    // Gate list ok → consume ok → usage re-read ok → inventory re-read fails.
    mock.push_credits(200, LIVE_CREDITS);
    mock.push_consume(200, r#"{"code":"reset","windows_reset":2}"#);
    mock.push_usage(200, LIVE_USAGE);
    mock.push_credits(500, "inventory on fire");

    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(status, 200, "redemption succeeded: {body}");
    assert_eq!(body["outcome"], "reset", "{body}");
    let warning = body["refresh_warning"]
        .as_str()
        .expect("a failed inventory re-read must surface as a warning");
    assert!(
        !warning.contains("on fire") && !warning.contains("at-cx"),
        "sanitized: {warning}"
    );
}

/// §Account identity/generation safety: a credential REPLACED during the IO
/// cannot receive the in-flight result — even when the replacement carries the
/// same upstream account id. Name and upstream identity alone are not enough.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credential_replaced_mid_flight_discards_the_observation() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;
    seed_exhausted(&proxy.pool, "cx");
    mock.push_usage_delayed(200, LIVE_USAGE, 250);

    let refresh = tokio::spawn(post_admin_owned(
        proxy.url("/llmux/refresh-usage"),
        serde_json::json!({"account": "cx"}),
    ));
    // Swap the credential mid-request: SAME name, SAME upstream account id,
    // different secret — a re-login / replacement.
    tokio::time::sleep(Duration::from_millis(80)).await;
    proxy.pool.reload_accounts(&[AccountConfig {
        name: "cx".into(),
        credential: AccountCredential::Codex {
            account_id: "acct-cx".into(),
            access_token: "at-cx-replacement".into(),
            refresh_token: "rt-cx-replacement".into(),
            expires_at_ms: far_future_ms(),
            last_refresh_ms: None,
        },
    }]);

    let (status, body) = refresh.await.expect("refresh task");
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["results"][0]["ok"], false,
        "an observation read with the old credential must be discarded: {body}"
    );

    let cx = dashboard_account(&proxy, "cx").await;
    let seven = seven_day_utilization(&cx).expect("window");
    assert!(
        (seven - 1.0).abs() < 1e-9,
        "the replaced account keeps its own state, not the predecessor's reading: {cx}"
    );
}

/// §Account identity/generation safety + §Crash boundary: a pending
/// redemption belongs to the credential that started it. After that credential
/// is REPLACED behind the same name, the old receipt must never be advertised
/// as the successor's retry, and its request id must never be posted under the
/// successor's credentials — not in this process, and not after a restart that
/// reloads the receipt from disk. The evidence itself is retained, not quietly
/// deleted.
#[tokio::test]
async fn replaced_credential_cannot_inherit_or_resurrect_a_pending_redemption() {
    let mock = WhamMock::spawn().await;
    let state = TempDir::new();
    let receipts = state.path().join("usage-resets.json");
    let proxy = Proxy::spawn_with_receipts(
        config_for(&mock, vec![codex_account("cx")]),
        Some(receipts.clone()),
    )
    .await;

    // 1. cx (acct-cx) leaves an UNCERTAIN redemption behind.
    mock.push_consume(200, r#"{"code":"teleported"}"#);
    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-old","confirm":true}),
    )
    .await;
    assert_eq!(status, 502, "uncertain: {body}");

    // 2. The account behind the name is REPLACED by a different upstream one.
    let successor = AccountConfig {
        name: "cx".into(),
        credential: AccountCredential::Codex {
            account_id: "acct-successor".into(),
            access_token: "at-successor".into(),
            refresh_token: "rt-successor".into(),
            expires_at_ms: far_future_ms(),
            last_refresh_ms: None,
        },
    };
    proxy.pool.reload_accounts(std::slice::from_ref(&successor));

    // 3. Both this process and a RESTARTED daemon reading the same receipt
    //    file must treat the old receipt identically.
    let restarted = Proxy::spawn_with_receipts(
        config_for(&mock, vec![successor.clone()]),
        Some(receipts.clone()),
    )
    .await;
    for (label, daemon) in [("same process", &proxy), ("after restart", &restarted)] {
        // 3a. The successor is never told it has a pending redemption.
        let (status, listed) = get_admin(daemon, "/llmux/reset-credits?account=cx").await;
        assert_eq!(status, 200, "{label}: {listed}");
        assert!(
            listed["pending_request_id"].is_null(),
            "{label}: a replaced credential must not inherit rid-old: {listed}"
        );
        let cx = dashboard_account(daemon, "cx").await;
        assert!(
            cx["usage_control"]["pending_request_id"].is_null(),
            "{label}: metadata must not advertise the predecessor's pending: {cx}"
        );

        // 3b. The old id cannot be retried under the new credentials.
        let (status, body) = post_admin(
            daemon,
            "/llmux/reset-credits/consume",
            serde_json::json!({"account":"cx","redeem_request_id":"rid-old","confirm":true}),
        )
        .await;
        assert!(
            status == 400 || status == 409,
            "{label}: the predecessor's request id must be refused, got {status}: {body}"
        );
        assert!(
            !posts(&mock)
                .iter()
                .any(|p| p.body.contains("rid-old")
                    && p.account_id.as_deref() == Some("acct-successor")),
            "{label}: rid-old must never be posted under the successor's credentials"
        );
    }

    // 4. The successor may still redeem with ITS OWN key…
    mock.push_consume(200, r#"{"code":"reset","windows_reset":1}"#);
    let (status, body) = post_admin(
        &restarted,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-new","confirm":true}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the successor's own redemption proceeds: {body}"
    );
    assert_eq!(body["outcome"], "reset", "{body}");

    // …and the predecessor's receipt is still on record as evidence, flagged,
    // rather than silently erased by the successor's write.
    let persisted: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(&receipts).expect("receipt file")).expect("json");
    let old = persisted
        .iter()
        .find(|p| p["request_id"] == "rid-old")
        .unwrap_or_else(|| panic!("rid-old evidence retained, got {persisted:?}"));
    assert_eq!(old["identity"], "codex:acct-cx", "{old}");
    assert!(
        old["invalidated_at_ms"].as_u64().is_some(),
        "the predecessor's receipt is marked invalid, not left live: {old}"
    );
}

/// §Account identity/generation safety: a ROSTER change is itself the
/// invalidation event. Removing an account must orphan nothing, and a
/// same-name successor must start blank — no inherited reset counts, credit
/// list or pending redemption — without waiting for a refresh to discover it.
#[tokio::test]
async fn roster_replacement_immediately_clears_inherited_control_state() {
    let mock = WhamMock::spawn().await;
    let state = TempDir::new();
    let receipts = state.path().join("usage-resets.json");
    let proxy = Proxy::spawn_with_receipts(
        config_for(&mock, vec![codex_account("cx")]),
        Some(receipts.clone()),
    )
    .await;

    // Cache real control state: counts (3), a credit list, and a pending id.
    let (_, refreshed) = post_admin(
        &proxy,
        "/llmux/refresh-usage",
        serde_json::json!({"account":"cx"}),
    )
    .await;
    assert_eq!(refreshed["results"][0]["ok"], true, "{refreshed}");
    let (_, listed) = get_admin(&proxy, "/llmux/reset-credits?account=cx").await;
    assert_eq!(listed["available_count"], 3, "{listed}");
    mock.push_consume(200, r#"{"code":"teleported"}"#);
    let (status, _) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-old","confirm":true}),
    )
    .await;
    assert_eq!(status, 502, "uncertain redemption recorded");
    let cx = dashboard_account(&proxy, "cx").await;
    assert_eq!(cx["usage_control"]["available_resets"], 3, "{cx}");
    assert_eq!(cx["usage_control"]["pending_request_id"], "rid-old", "{cx}");

    // Remove the account through the daemon's own roster path.
    let (status, body) = post_admin(
        &proxy,
        "/llmux/remove-account",
        serde_json::json!({"name":"cx","confirm":true}),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    // Re-add the SAME NAME as a different account. It must inherit nothing.
    let (status, body) = post_admin(
        &proxy,
        "/llmux/add-account",
        serde_json::json!({"name":"cx","api_key":"sk-ant-successor"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let cx = dashboard_account(&proxy, "cx").await;
    assert!(
        cx["usage_control"]["available_resets"].is_null(),
        "a same-name successor must not inherit reset counts: {cx}"
    );
    assert!(
        cx["usage_control"]["credits"].is_null()
            || cx["usage_control"]["credits"]
                .as_array()
                .is_some_and(Vec::is_empty),
        "…nor the predecessor's credit list: {cx}"
    );
    assert!(
        cx["usage_control"]["pending_request_id"].is_null(),
        "…nor its pending redemption: {cx}"
    );

    // The predecessor's receipt survives as FLAGGED evidence, and its id is
    // refused for the successor (which is not even a codex account now).
    let persisted: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(&receipts).expect("receipt file")).expect("json");
    let old = persisted
        .iter()
        .find(|p| p["request_id"] == "rid-old")
        .unwrap_or_else(|| panic!("rid-old evidence retained, got {persisted:?}"));
    assert!(
        old["invalidated_at_ms"].as_u64().is_some(),
        "the roster change invalidates it durably: {old}"
    );
}

/// The two hazards in ONE interleaving (`.prd/16-codex-usage-controls.md`
/// §Account identity/generation safety + §Crash boundary): an account with an
/// UNRESOLVED redemption has a usage read in flight when its credential is
/// replaced. The neighbouring tests cover each half — a delayed refresh with no
/// prior receipt, and a replacement with no in-flight read — and neither pins
/// what the two do to each other.
///
/// The order is forced, not raced: the mock announces the GET has arrived (so
/// the daemon has already captured the predecessor's credential) and withholds
/// the response until the test says so. No sleeps.
#[tokio::test]
async fn pending_receipt_is_invalidated_when_a_replacement_lands_mid_refresh() {
    let mock = WhamMock::spawn().await;
    let state = TempDir::new();
    let receipts = state.path().join("usage-resets.json");
    let proxy = Proxy::spawn_with_receipts(
        config_for(&mock, vec![codex_account("cx")]),
        Some(receipts.clone()),
    )
    .await;

    // 1. cx (acct-cx) is left holding an UNRESOLVED redemption.
    mock.push_consume(200, r#"{"code":"teleported"}"#);
    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-old","confirm":true}),
    )
    .await;
    assert_eq!(status, 502, "uncertain: {body}");
    let posts_before = posts(&mock).len();
    assert_eq!(posts_before, 1, "exactly one redemption so far");

    // 2. A usage refresh starts under the PREDECESSOR and is frozen mid-flight.
    let (arrived, release) = mock.gate_usage();
    let refresh = tokio::spawn(post_admin_owned(
        proxy.url("/llmux/refresh-usage"),
        serde_json::json!({"account": "cx"}),
    ));
    arrived.await.expect("the usage GET reached upstream");

    // 3. The credential behind the name is replaced WHILE that read is open.
    let successor = AccountConfig {
        name: "cx".into(),
        credential: AccountCredential::Codex {
            account_id: "acct-successor".into(),
            access_token: "at-successor".into(),
            refresh_token: "rt-successor".into(),
            expires_at_ms: far_future_ms(),
            last_refresh_ms: None,
        },
    };
    proxy.pool.reload_accounts(std::slice::from_ref(&successor));

    // 4. Only now does the predecessor's read complete.
    release.send(()).expect("release the usage response");
    let (status, body) = refresh.await.expect("refresh task");
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["results"][0]["ok"], false,
        "the predecessor's reading must be discarded: {body}"
    );

    // 5a. The receipt is DURABLY invalidated — evidence kept, authority gone.
    let persisted: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(&receipts).expect("receipt file")).expect("json");
    let old = persisted
        .iter()
        .find(|p| p["request_id"] == "rid-old")
        .unwrap_or_else(|| panic!("rid-old evidence retained, got {persisted:?}"));
    assert_eq!(old["identity"], "codex:acct-cx", "{old}");
    assert!(
        old["invalidated_at_ms"].as_u64().is_some(),
        "the in-flight replacement must invalidate it durably: {old}"
    );

    // 5b. The successor is told nothing about it — on either read surface.
    let cx = dashboard_account(&proxy, "cx").await;
    assert!(
        cx["usage_control"]["pending_request_id"].is_null(),
        "the successor must not inherit the pending id: {cx}"
    );
    let (status, listed) = get_admin(&proxy, "/llmux/reset-credits?account=cx").await;
    assert_eq!(status, 200, "{listed}");
    assert!(
        listed["pending_request_id"].is_null(),
        "…nor see it in the reset list: {listed}"
    );

    // 5c. And the predecessor's id can never be spent under the successor.
    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-old","confirm":true}),
    )
    .await;
    assert!(
        status == 400 || status == 409,
        "rid-old must be refused for the successor, got {status}: {body}"
    );
    assert_eq!(
        posts(&mock).len(),
        posts_before,
        "no further redemption was attempted at all"
    );
    assert!(
        !posts(&mock)
            .iter()
            .any(|p| p.account_id.as_deref() == Some("acct-successor")),
        "nothing was ever posted under the successor's credentials"
    );
}

/// §Crash boundary: if the receipt cannot be saved, NO redemption is attempted.
#[tokio::test]
async fn unsaveable_receipt_prevents_the_redemption() {
    let mock = WhamMock::spawn().await;
    let state = TempDir::new();
    // A FILE where the receipt directory must be → create_dir_all/write fails.
    let blocker = state.path().join("blocked");
    std::fs::write(&blocker, b"not a directory").expect("seed blocker");
    let proxy = Proxy::spawn_with_receipts(
        config_for(&mock, vec![codex_account("cx")]),
        Some(blocker.join("usage-resets.json")),
    )
    .await;

    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(status, 500, "{body}");
    assert!(posts(&mock).is_empty(), "no redemption without a receipt");
}

/// §Per-account exclusion: two different keys racing the same account never
/// both reach upstream — the loser is 409/busy, not queued.
#[tokio::test]
async fn competing_consume_keys_never_both_reach_upstream() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;
    // Both attempts block on a slow inventory read, guaranteeing overlap.
    mock.push_credits_delayed(200, LIVE_CREDITS, 300);
    mock.push_credits_delayed(200, LIVE_CREDITS, 300);

    let one = post_admin_owned(
        proxy.url("/llmux/reset-credits/consume"),
        serde_json::json!({"account":"cx","redeem_request_id":"rid-a","confirm":true}),
    );
    let two = post_admin_owned(
        proxy.url("/llmux/reset-credits/consume"),
        serde_json::json!({"account":"cx","redeem_request_id":"rid-b","confirm":true}),
    );
    let (a, b) = tokio::join!(one, two);
    let statuses = [a.0, b.0];
    assert!(
        statuses.contains(&409),
        "the competing operation is refused, not queued: {statuses:?}"
    );
    assert!(
        posts(&mock).len() <= 1,
        "at most one redemption reached upstream: {:?}",
        posts(&mock)
    );
}

/// §Reset action gate: without a fresh, positive inventory there is no NEW
/// redemption — and the gate failure never reaches the POST.
#[tokio::test]
async fn new_redemption_requires_a_fresh_available_credit() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;

    mock.push_credits(200, r#"{"available_count":0,"credits":[]}"#);
    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(status, 409, "zero available: {body}");

    mock.push_credits(
        200,
        r#"{"available_count":2,"credits":[{"id":"c9","reset_type":"codex_rate_limits","status":"redeemed"}]}"#,
    );
    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-2","confirm":true}),
    )
    .await;
    assert_eq!(status, 409, "no row with status available: {body}");

    mock.push_credits(500, "nope");
    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-3","confirm":true}),
    )
    .await;
    assert_eq!(status, 502, "unknown inventory is not permission: {body}");

    assert!(posts(&mock).is_empty(), "the gate precedes the POST");
}

/// §Reset action gate: the gate is a POSITIVE available count. An UNKNOWN
/// count is not permission, even when the list carries an available-looking
/// row — the count is the upstream's own answer to "may this be spent".
#[tokio::test]
async fn unknown_available_count_is_not_permission_to_redeem() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;
    mock.push_credits(
        200,
        r#"{"credits":[{"id":"c1","reset_type":"codex_rate_limits","status":"available"}]}"#,
    );

    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(
        status, 409,
        "unknown owned count blocks a new redemption: {body}"
    );
    assert!(posts(&mock).is_empty(), "and never reaches the POST");
}

/// §Account identity/generation safety: the INVENTORY read is IO too. A
/// credential replaced while it is in flight must not have its result
/// published, must not produce a receipt, and must not be spent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credential_replaced_during_inventory_read_blocks_the_redemption() {
    let mock = WhamMock::spawn().await;
    let state = TempDir::new();
    let receipts = state.path().join("usage-resets.json");
    let proxy = Proxy::spawn_with_receipts(
        config_for(&mock, vec![codex_account("cx")]),
        Some(receipts.clone()),
    )
    .await;
    mock.push_credits_delayed(200, LIVE_CREDITS, 250);

    let attempt = tokio::spawn(post_admin_owned(
        proxy.url("/llmux/reset-credits/consume"),
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    ));
    tokio::time::sleep(Duration::from_millis(80)).await;
    proxy.pool.reload_accounts(&[AccountConfig {
        name: "cx".into(),
        credential: AccountCredential::Codex {
            account_id: "acct-successor".into(),
            access_token: "at-successor".into(),
            refresh_token: "rt-successor".into(),
            expires_at_ms: far_future_ms(),
            last_refresh_ms: None,
        },
    }]);

    let (status, body) = attempt.await.expect("consume task");
    assert!(
        status >= 400,
        "a redemption whose account changed mid-read must not proceed, got {status}: {body}"
    );
    assert!(
        posts(&mock).is_empty(),
        "no redemption was spent: {:?}",
        posts(&mock)
    );
    assert!(
        !receipts.exists()
            || std::fs::read_to_string(&receipts)
                .expect("receipts")
                .contains("\"account\""),
        "no receipt may be filed under a credential that is already gone"
    );
    let listed: Vec<serde_json::Value> = std::fs::read(&receipts)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    assert!(
        listed.is_empty(),
        "no receipt at all should have been written: {listed:?}"
    );
}

/// §Crash boundary: after a restart the operator must be able to SEE the
/// pending redemption (and its id) from the read surfaces alone — discovering
/// it by attempting another consume would be exactly the second spend the
/// receipt exists to prevent.
#[tokio::test]
async fn restart_exposes_the_pending_id_without_attempting_a_consume() {
    let mock = WhamMock::spawn().await;
    let state = TempDir::new();
    let receipts = state.path().join("usage-resets.json");
    mock.push_consume(200, r#"{"code":"teleported"}"#);
    let first = Proxy::spawn_with_receipts(
        config_for(&mock, vec![codex_account("cx")]),
        Some(receipts.clone()),
    )
    .await;
    let (status, _) = post_admin(
        &first,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(status, 502, "uncertain");
    let posts_before = posts(&mock).len();

    let restarted =
        Proxy::spawn_with_receipts(config_for(&mock, vec![codex_account("cx")]), Some(receipts))
            .await;
    // The dashboard alone — no control call of any kind — must show it.
    let cx = dashboard_account(&restarted, "cx").await;
    assert_eq!(
        cx["usage_control"]["pending_request_id"], "rid-1",
        "a reloaded receipt is visible on the read surface: {cx}"
    );
    assert_eq!(
        posts(&mock).len(),
        posts_before,
        "…and nothing was redeemed to discover it"
    );
}

/// §Crash boundary / R1: when upstream returns a TERMINAL outcome but the
/// durable receipt cannot be released, the outcome stays terminal WITH a
/// warning and the hold is RETAINED — a client must never be told the hold is
/// gone while the blocking receipt is still on disk.
#[cfg(unix)]
#[tokio::test]
async fn terminal_outcome_with_a_failed_receipt_release_retains_the_hold() {
    use std::os::unix::fs::PermissionsExt as _;

    let mock = WhamMock::spawn().await;
    let state = TempDir::new();
    let dir = state.path().join("receipts");
    std::fs::create_dir_all(&dir).expect("receipt dir");
    let receipts = dir.join("usage-resets.json");
    let proxy = Proxy::spawn_with_receipts(
        config_for(&mock, vec![codex_account("cx")]),
        Some(receipts.clone()),
    )
    .await;

    mock.push_consume(200, r#"{"code":"teleported"}"#);
    let (status, _) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(status, 502, "uncertain hold established");

    // Make the receipt directory read-only: the release cannot be written.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("chmod");

    mock.push_consume(200, r#"{"code":"already_redeemed","windows_reset":1}"#);
    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");

    assert_eq!(status, 200, "the upstream outcome is terminal: {body}");
    assert_eq!(body["outcome"], "already_redeemed", "{body}");
    assert!(
        body["refresh_warning"]
            .as_str()
            .is_some_and(|w| w.contains("record") || w.contains("pending")),
        "the failed release is reported: {body}"
    );
    assert_eq!(
        body["usage_control"]["pending_request_id"], "rid-1",
        "the hold is RETAINED while the durable receipt still blocks: {body}"
    );
    let cx = dashboard_account(&proxy, "cx").await;
    assert_eq!(
        cx["usage_control"]["pending_request_id"], "rid-1",
        "…and the read surface agrees: {cx}"
    );
    assert!(
        std::fs::read_to_string(&receipts)
            .expect("receipts")
            .contains("rid-1"),
        "the blocking receipt is still on disk"
    );
}

/// §Reset action gate: `applicable_available_count == 0` is server-reported
/// INFORMATION, not a refusal — the attempt proceeds with a warning.
#[tokio::test]
async fn zero_applicable_count_warns_but_does_not_block() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;
    // The live usage body reports owned 3 / applicable 0.
    let (_, refreshed) = post_admin(
        &proxy,
        "/llmux/refresh-usage",
        serde_json::json!({"account":"cx"}),
    )
    .await;
    assert_eq!(refreshed["results"][0]["ok"], true, "{refreshed}");

    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(status, 200, "applicability is not a gate: {body}");
    let warning = body["applicability_warning"]
        .as_str()
        .expect("applicability warning");
    assert!(
        warning.contains('3') && warning.contains('0'),
        "warning names owned and applicable counts: {warning}"
    );
}

/// Trace S2.3/S2.6: all four upstream codes map to distinct outcomes; none
/// masquerades as a success.
#[tokio::test]
async fn all_four_reset_outcomes_are_distinct() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;
    for (code, expected) in [
        ("reset", "reset"),
        ("already_redeemed", "already_redeemed"),
        ("nothing_to_reset", "nothing_to_reset"),
        ("no_credit", "no_credit"),
    ] {
        mock.push_consume(200, &format!(r#"{{"code":"{code}","windows_reset":0}}"#));
        let (status, body) = post_admin(
            &proxy,
            "/llmux/reset-credits/consume",
            serde_json::json!({
                "account": "cx", "redeem_request_id": format!("rid-{code}"), "confirm": true
            }),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["outcome"], expected, "{code}: {body}");
    }
}

/// Trace S2.3/S2.5: an unknown code is an UNCERTAIN error that keeps the
/// caller's request id — never a fabricated success.
#[tokio::test]
async fn unknown_outcome_is_an_error_that_retains_the_request_id() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;
    mock.push_consume(200, r#"{"code":"teleported","windows_reset":0}"#);

    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-x","confirm":true}),
    )
    .await;
    assert_eq!(status, 502, "uncertain, not success: {body}");
    let message = body.to_string();
    assert!(
        message.contains("rid-x"),
        "request id retained for explicit retry: {message}"
    );
}

/// Trace S2.2/S2.5: missing confirmation, a blank request id and a non-codex
/// account are all refused BEFORE the network.
#[tokio::test]
async fn consume_refuses_unconfirmed_blank_and_unsupported_before_network() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx"), apikey_account("k")]).await;

    let (status, _) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":false}),
    )
    .await;
    assert_eq!(status, 400, "confirmation required");

    let (status, _) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"  ","confirm":true}),
    )
    .await;
    assert_eq!(status, 400, "blank request id refused");

    let (status, _) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"k","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(status, 422, "non-codex account cannot redeem");

    let (status, _) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"nope","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(status, 404, "unknown account");

    assert!(
        mock.seen().is_empty(),
        "no redemption attempted: {:?}",
        mock.seen_paths()
    );
}

/// Trace S2.4: a successful redemption whose follow-up read fails is a
/// SUCCESS with a stale-read warning — not a retryable redemption failure.
#[tokio::test]
async fn post_consume_refresh_failure_is_a_warning_not_a_failed_redemption() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx")]).await;
    mock.push_consume(200, r#"{"code":"reset","windows_reset":2}"#);
    mock.push_usage(500, "upstream on fire");

    let (status, body) = post_admin(
        &proxy,
        "/llmux/reset-credits/consume",
        serde_json::json!({"account":"cx","redeem_request_id":"rid-1","confirm":true}),
    )
    .await;
    assert_eq!(status, 200, "redemption succeeded: {body}");
    assert_eq!(body["outcome"], "reset", "{body}");
    let warning = body["refresh_warning"].as_str().expect("warning present");
    assert!(
        !warning.contains("at-cx"),
        "no credential in warning: {warning}"
    );
    assert!(
        !warning.contains("on fire"),
        "no raw upstream body in warning: {warning}"
    );
}

// ---------------------------------------------------------------------------
// S3 — manual switch
// ---------------------------------------------------------------------------

/// Trace S3.3/S3.4: a manual switch refreshes the target first, so the commit
/// ranks on fresh data; the already-exhausted cache is replaced.
#[tokio::test]
async fn manual_switch_refreshes_target_before_commit() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx"), codex_account("cy")]).await;
    seed_exhausted(&proxy.pool, "cy");

    let (status, body) =
        post_admin(&proxy, "/llmux/switch", serde_json::json!({"account":"cy"})).await;
    assert_eq!(
        status, 200,
        "switch to a stale over-threshold target: {body}"
    );
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["current"], "cy", "{body}");

    let seen = mock.seen();
    assert_eq!(seen.len(), 1, "the target was refreshed: {seen:?}");
    assert_eq!(seen[0].path, "/wham/usage");
    assert_eq!(seen[0].account_id.as_deref(), Some("acct-cy"));

    let cy = dashboard_account(&proxy, "cy").await;
    let seven = seven_day_utilization(&cy).expect("window");
    assert!((seven - 0.43).abs() < 1e-9, "fresh reading committed: {cy}");
}

/// Trace S3.5: a failed refresh is reported as a warning next to a successful
/// switch — never silent success, never fictional fresh data.
#[tokio::test]
async fn switch_reports_refresh_failure_as_a_separate_warning() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![codex_account("cx"), codex_account("cy")]).await;
    seed_exhausted(&proxy.pool, "cy");
    mock.push_usage(503, "nope");

    let (status, body) =
        post_admin(&proxy, "/llmux/switch", serde_json::json!({"account":"cy"})).await;
    assert_eq!(status, 200, "switch still works: {body}");
    assert_eq!(body["current"], "cy", "{body}");
    assert!(
        body["refresh_warning"].as_str().is_some(),
        "refresh failure surfaced separately: {body}"
    );

    let cy = dashboard_account(&proxy, "cy").await;
    let seven = seven_day_utilization(&cy).expect("window retained");
    assert!(
        (seven - 1.0).abs() < 1e-9,
        "failed refresh never clears the prior window: {cy}"
    );
}

/// Trace S3.4/S3.7: a paused account is still refused by the pool — the
/// pre-switch refresh does not unpause anything.
#[tokio::test]
async fn paused_target_is_still_refused_after_refresh() {
    let mock = WhamMock::spawn().await;
    let mut config = config_for(&mock, vec![codex_account("cx"), codex_account("cy")]);
    config.paused_accounts.insert("cy".into());
    let proxy = Proxy::spawn_config(config).await;

    let (status, body) =
        post_admin(&proxy, "/llmux/switch", serde_json::json!({"account":"cy"})).await;
    assert_eq!(status, 409, "paused target refused: {body}");

    let cy = dashboard_account(&proxy, "cy").await;
    assert_eq!(cy["paused"], true, "still paused: {cy}");
}

/// Trace S3.4: switching to an account whose provider has no usage control
/// keeps the existing switch semantics (no refusal, no upstream call).
#[tokio::test]
async fn unsupported_provider_switch_is_unchanged() {
    let mock = WhamMock::spawn().await;
    let proxy = Proxy::spawn(&mock, vec![apikey_account("k"), codex_account("cx")]).await;

    let (status, body) =
        post_admin(&proxy, "/llmux/switch", serde_json::json!({"account":"k"})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["current"], "k", "{body}");
    assert!(
        mock.seen().is_empty(),
        "no usage call for an unsupported provider: {:?}",
        mock.seen_paths()
    );
}
