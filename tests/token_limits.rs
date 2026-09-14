//! Wire acceptance for the explicit output cap (`docs/keys-history/spec.md`
//! trace T): what actually leaves llmux's socket when a client sends
//! `max_tokens`.
//!
//! The unit tests in `provider::responses_request` cover the translation in
//! isolation; what only a running proxy can show is the byte on the wire — so
//! every assertion here reads the REQUEST BODY the mock upstream received, not
//! a translator return value.
//!
//! The two backends differ, and the difference is upstream's, not llmux's:
//!
//! - **grok** accepts `max_output_tokens` (probes 2026-09-11 and 2026-09-14:
//!   cap 1 → 200 `status:incomplete`/`max_output_tokens`), so the client's cap
//!   is forwarded verbatim, down to 1.
//! - **codex** refuses the parameter outright (same probes:
//!   `400 {"detail":"Unsupported parameter: max_output_tokens"}`), so the cap
//!   is omitted and REPORTED as a loss rather than faked locally. Whether any
//!   other field can carry a cap to that backend is an open research question;
//!   until it has a receipt, this file pins the omission that actually ships.
//!
//! Scope limit stated up front: the upstream is a MOCK that answers whatever
//! each test scripts. These tests therefore prove what llmux PUTS ON THE WIRE
//! and how it handles the reply — never that any backend honors, enforces, or
//! bills a cap. The only claims about real backends here are the dated probes
//! cited above, and they live in `docs/responses-compatibility/spec.md` §1.
//!
//! Isolation: every test owns its mock, its proxy (port 0) and a tempdir
//! config — no env mutation, nothing touches the real `~/.config`.

#[path = "mock_upstream.rs"]
mod mock_upstream;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use llmux::config::{self, AccountConfig, AccountCredential, Config};
use llmux::proxy::server::{serve, AppState};
use llmux::scheduler::AccountPool;
use mock_upstream::{MockUpstream, ScriptedResponse};

/// Minimal Responses SSE: one text delta, then completion with usage — enough
/// for a 200 on either client leg without dragging tool-call bookkeeping into
/// tests that are about the REQUEST side.
const RESPONSES_SSE: &str = concat!(
    "event: response.created\n",
    r#"data: {"type":"response.created","response":{"id":"resp_cap"}}"#,
    "\n\n",
    "event: response.output_item.added\n",
    r#"data: {"type":"response.output_item.added","item":{"type":"message","role":"assistant"}}"#,
    "\n\n",
    "event: response.output_text.delta\n",
    r#"data: {"type":"response.output_text.delta","delta":"ok"}"#,
    "\n\n",
    "event: response.output_item.done\n",
    r#"data: {"type":"response.output_item.done","item":{"type":"message"}}"#,
    "\n\n",
    "event: response.completed\n",
    r#"data: {"type":"response.completed","response":{"id":"resp_cap","usage":{"input_tokens":9,"output_tokens":1}}}"#,
    "\n\n",
);

/// Self-cleaning unique temp dir (no tempfile dev-dependency), as in `e2e.rs`.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "llmux-token-limits-{}-{}",
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

/// Beyond the background-refresh window, so no test's mock queue is disturbed
/// by a token refresh running behind its back.
fn far_future_ms() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64;
    now + 24 * 3_600 * 1_000
}

/// The two Responses-family backends the cap contract covers.
#[derive(Clone, Copy, Debug)]
enum Flavor {
    Codex,
    Grok,
}

impl Flavor {
    fn account(self, mock: &MockUpstream) -> AccountConfig {
        let (name, credential) = match self {
            Self::Codex => (
                "cx",
                AccountCredential::Codex {
                    account_id: "acct-cap".into(),
                    access_token: "at-cap".into(),
                    refresh_token: "rt-cap".into(),
                    expires_at_ms: far_future_ms(),
                    last_refresh_ms: None,
                },
            ),
            Self::Grok => (
                "gk",
                AccountCredential::Grok {
                    subject: "sub-cap".into(),
                    access_token: "at-cap".into(),
                    refresh_token: "rt-cap".into(),
                    expires_at_ms: far_future_ms(),
                    token_endpoint: format!("{}/v1/oauth/token", mock.base_url()),
                    last_refresh_ms: None,
                },
            ),
        };
        AccountConfig {
            name: name.to_string(),
            credential,
        }
    }

    /// Single-account config with this flavor's upstream pointed at the mock.
    fn config(self, mock: &MockUpstream) -> Config {
        let mut config = Config {
            upstream: mock.base_url(),
            accounts: vec![self.account(mock)],
            ..Default::default()
        };
        config.codex.upstream = mock.base_url();
        config.codex.token_url = format!("{}/v1/oauth/token", mock.base_url());
        config.grok.upstream = mock.base_url();
        // Single-flavor pool driven through the provider path, as in the e2e
        // compatibility scenarios.
        config.routing.enabled = false;
        config
    }
}

/// One running proxy over a tempdir config, listening on an OS-assigned port.
struct Proxy {
    addr: SocketAddr,
    _tmp: TempDir,
}

impl Proxy {
    async fn spawn(mut config: Config) -> Self {
        // The always-on idle probe would otherwise fire its own background
        // request into this test's scripted mock queue.
        config.proxy.idle_probe.enabled = false;
        config.proxy.port = 0; // OS-assigned; `serve` reports it via `ready`
        let tmp = TempDir::new();
        let config_path = tmp.path().join("llmux.json");
        config::save_path(&config_path, &config).expect("seed config");

        let pool = AccountPool::new(&config.accounts);
        let mut state = AppState::new(config, pool, None, None).expect("app state");
        // Keep every persisted side effect inside the tempdir.
        state.config_path = Some(config_path);
        state.activity_log_path = Some(tmp.path().join("activity.jsonl"));
        state.raw_io_path = Some(tmp.path().join("raw-io.jsonl"));

        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(serve(state, Some(ready_tx)));
        let addr = ready_rx.await.expect("proxy ready");
        Self { addr, _tmp: tmp }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.addr.port(), path)
    }
}

/// A one-token request: the user clause this contract exists for.
fn capped_request(stream: bool) -> String {
    serde_json::json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 1,
        "stream": stream,
        "messages": [{"role": "user", "content": "hi"}],
    })
    .to_string()
}

async fn post_messages(proxy: &Proxy, body: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(proxy.url("/v1/messages"))
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .body(body.to_string())
        .send()
        .await
        .expect("proxy reachable")
}

/// A comma-separated compatibility header as a list (`None` → empty).
fn header_list(response: &reqwest::Response, name: &str) -> Vec<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Trace T3/T4 on the wire for the backend that ACCEPTS a cap: `max_tokens: 1`
/// leaves llmux as `max_output_tokens: 1` on ONE upstream request, on both
/// client legs. The cap is neither raised to some "usable" floor nor split
/// into a second, uncapped call.
async fn grok_forwards_the_cap_verbatim(stream: bool) {
    let mock = MockUpstream::spawn().await;
    mock.push(ScriptedResponse::sse_plain(RESPONSES_SSE, 19));
    let proxy = Proxy::spawn(Flavor::Grok.config(&mock)).await;

    let response = post_messages(&proxy, &capped_request(stream)).await;
    assert_eq!(response.status(), 200, "grok serves the capped turn");

    // Trace T5: the caveat the client is owed rides on the response as a
    // WARNING, never as a claimed omission — the field really is on the wire.
    let warnings = header_list(&response, "x-llmux-compatibility-warnings");
    assert!(
        warnings.iter().any(|w| w == "max_tokens_semantics"),
        "grok warns that the cap's upstream meaning is unproven: {warnings:?}"
    );
    let omitted = header_list(&response, "x-llmux-omitted-fields");
    assert!(
        !omitted.iter().any(|f| f == "max_tokens"),
        "grok must not report the forwarded cap as dropped: {omitted:?}"
    );

    let seen = mock.seen();
    assert_eq!(
        seen.len(),
        1,
        "exactly one upstream call (no capless retry)"
    );
    assert_eq!(seen[0].path, "/responses");
    let upstream: serde_json::Value = serde_json::from_slice(&seen[0].body).expect("upstream json");
    assert_eq!(
        upstream["max_output_tokens"].as_u64(),
        Some(1),
        "grok sends the client's cap unchanged: {upstream}"
    );
    assert!(
        upstream.get("max_tokens").is_none(),
        "…under the Responses name only: {upstream}"
    );
}

#[tokio::test]
async fn grok_stream_sends_max_output_tokens_one() {
    grok_forwards_the_cap_verbatim(true).await;
}

#[tokio::test]
async fn grok_nonstream_sends_max_output_tokens_one() {
    grok_forwards_the_cap_verbatim(false).await;
}

/// The codex contract as it SHIPS: the backend rejects `max_output_tokens`, so
/// no cap reaches the wire and the client is TOLD (omission + warning headers)
/// rather than being served a silently uncapped turn it believes it bounded.
///
/// This is the behavior the "codex ignores max_tokens" caveat documents. If a
/// supported cap field is ever found for that backend, this test is the one
/// that must flip first.
async fn codex_omits_the_cap_and_reports_it(stream: bool) {
    let mock = MockUpstream::spawn().await;
    mock.push(ScriptedResponse::sse_plain(RESPONSES_SSE, 19));
    let proxy = Proxy::spawn(Flavor::Codex.config(&mock)).await;

    let response = post_messages(&proxy, &capped_request(stream)).await;
    assert_eq!(response.status(), 200, "codex serves the turn");

    let omitted = header_list(&response, "x-llmux-omitted-fields");
    assert!(
        omitted.iter().any(|f| f == "max_tokens"),
        "the dropped cap is reported, never silent: {omitted:?}"
    );
    let warnings = header_list(&response, "x-llmux-compatibility-warnings");
    assert!(
        warnings.iter().any(|w| w == "max_tokens"),
        "…and every omission is also a warning: {warnings:?}"
    );

    let seen = mock.seen();
    assert_eq!(seen.len(), 1, "exactly one upstream call");
    let upstream: serde_json::Value = serde_json::from_slice(&seen[0].body).expect("upstream json");
    assert!(
        upstream.get("max_output_tokens").is_none(),
        "the codex backend 400s on this parameter, so it is not sent: {upstream}"
    );
    assert!(
        upstream.get("max_tokens").is_none(),
        "nor under the Anthropic name: {upstream}"
    );
}

#[tokio::test]
async fn codex_stream_omits_the_cap_and_reports_it() {
    codex_omits_the_cap_and_reports_it(true).await;
}

#[tokio::test]
async fn codex_nonstream_omits_the_cap_and_reports_it() {
    codex_omits_the_cap_and_reports_it(false).await;
}

/// Trace T4: when a capped turn draws a 4xx, llmux RELAYS it on the single
/// call it made and does not retry without the cap — a silently uncapped
/// second turn is exactly the failure the client asked llmux not to invent on
/// its behalf.
///
/// Read this receipt narrowly. The 400 is scripted BY THE MOCK, so it is
/// evidence about llmux's retry behavior and nothing else: no backend's cap
/// handling is probed here, no cap is enforced, and the status is deliberately
/// a generic upstream client error rather than a borrowed codex message — the
/// flavor under test is grok (the only one llmux sends a cap to), and dressing
/// its mock in codex's `Unsupported parameter` refusal would assert a
/// cross-backend fact this test never observed.
#[tokio::test]
async fn upstream_4xx_on_a_capped_turn_is_relayed_without_a_capless_retry() {
    let mock = MockUpstream::spawn().await;
    mock.push(ScriptedResponse::ClientError {
        status: 400,
        body: r#"{"error":{"type":"invalid_request","message":"scripted upstream refusal"}}"#
            .to_string(),
    });
    let proxy = Proxy::spawn(Flavor::Grok.config(&mock)).await;

    let response = post_messages(&proxy, &capped_request(false)).await;
    assert_eq!(response.status(), 400, "the client sees the refusal");

    let seen = mock.seen();
    assert_eq!(
        seen.len(),
        1,
        "exactly one upstream call — no retry without the cap: {seen:?}"
    );
    let upstream: serde_json::Value = serde_json::from_slice(&seen[0].body).expect("upstream json");
    assert_eq!(
        upstream["max_output_tokens"].as_u64(),
        Some(1),
        "and that one call carried the grok cap: {upstream}"
    );
}
