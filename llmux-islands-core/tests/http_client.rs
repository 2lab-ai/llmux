use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{header, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use llmux_islands_core::{
    Action, ClientConfig, ClientErrorKind, DaemonClient, Effect, EffectExecution, EventDraft,
    LoginStatus, MaintenanceCommand, OperationOutcome, OperationRequest, Provider, ReleaseChannel,
    SecretString,
};
use serde_json::{json, Value};

const CONTROL_KEY: &str = "ta-control-secret-value";
const ACCOUNT_KEY: &str = "sk-account-secret-value";

#[derive(Clone)]
enum MockMode {
    Normal,
    SlowDashboard,
    SecretError,
    TransientLoginFailure,
    LoginBodyError,
    RedirectDashboard { location: String },
    AddAccountNoChange,
}

#[derive(Clone)]
struct MockState {
    mode: MockMode,
    seen: Arc<Mutex<Vec<SeenRequest>>>,
}

#[derive(Debug)]
struct SeenRequest {
    method: Method,
    uri: String,
    api_key: Option<String>,
    body: Value,
}

async fn mock_api(State(state): State<MockState>, request: Request<Body>) -> Response {
    let method = request.method().clone();
    let uri = request
        .uri()
        .path_and_query()
        .map_or_else(|| request.uri().path().to_string(), ToString::to_string);
    let api_key = request
        .headers()
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let bytes = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .expect("mock body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("request JSON")
    };
    state.seen.lock().expect("seen lock").push(SeenRequest {
        method: method.clone(),
        uri: uri.clone(),
        api_key,
        body: body.clone(),
    });

    if matches!(state.mode, MockMode::SlowDashboard) && uri == "/llmux/dashboard" {
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    if matches!(state.mode, MockMode::SecretError) {
        return json_response(
            StatusCode::UNAUTHORIZED,
            json!({
                "type": "error",
                "error": {
                    "type": "authentication_error",
                    "message": "account alice@example.com rejected; password=hunter2-from-daemon",
                    "debug_body": ACCOUNT_KEY
                }
            }),
        );
    }
    if matches!(state.mode, MockMode::TransientLoginFailure)
        && uri.starts_with("/llmux/login/status?")
    {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "error": { "message": "login backend warming up" } }),
        );
    }
    if matches!(state.mode, MockMode::LoginBodyError) && uri.starts_with("/llmux/login/status?") {
        return json_response(
            StatusCode::OK,
            json!({
                "phase": "error",
                "error": "login failed for retired-daemon-account-42 / retired@example.com; password=hunter2"
            }),
        );
    }
    if let MockMode::RedirectDashboard { location } = &state.mode {
        if uri == "/llmux/dashboard" {
            return (
                StatusCode::TEMPORARY_REDIRECT,
                [(header::LOCATION, location.as_str())],
            )
                .into_response();
        }
    }
    if matches!(state.mode, MockMode::AddAccountNoChange) && uri == "/llmux/add-account" {
        return json_response(
            StatusCode::OK,
            json!({
                "ok": true,
                "name": body["name"],
                "type": "apikey",
                "added": false
            }),
        );
    }

    match (method.clone(), uri.as_str()) {
        (Method::GET, "/llmux/dashboard") => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            include_str!("../fixtures/dashboard-current.json"),
        )
            .into_response(),
        (Method::POST, "/llmux/add-account") => json_response(
            StatusCode::OK,
            json!({
                "ok": true,
                "name": body["name"].as_str().unwrap_or("api-1"),
                "type": "apikey",
                "added": true,
                "api_key_masked": "sk-***alue"
            }),
        ),
        (Method::POST, "/llmux/remove-account") => json_response(
            StatusCode::OK,
            json!({ "ok": true, "name": body["name"], "removed": true }),
        ),
        (Method::POST, "/llmux/pause-account") => json_response(
            StatusCode::OK,
            json!({ "ok": true, "account": body["account"], "paused": body["paused"] }),
        ),
        (Method::POST, "/llmux/settings") => json_response(
            StatusCode::OK,
            json!({ "ok": true, "email_anonymous": body["email_anonymous"] }),
        ),
        (Method::POST, "/llmux/events") => {
            let events = if body.get("remove").is_some() {
                Vec::new()
            } else {
                vec![body]
            };
            json_response(StatusCode::OK, json!({ "ok": true, "events": events }))
        }
        (Method::POST, "/llmux/login/start") => json_response(
            StatusCode::OK,
            json!({ "ok": true, "state": "daemon-state-1", "provider": body["provider"] }),
        ),
        (Method::GET, "/llmux/login/status?state=daemon-state-1") => json_response(
            StatusCode::OK,
            json!({
                "phase": "pending",
                "verification_uri": "https://x.ai/device?state=must-not-persist",
                "user_code": "ABCD-EFGH"
            }),
        ),
        (Method::POST, "/llmux/login/cancel") => {
            json_response(StatusCode::OK, json!({ "ok": true, "cancelled": true }))
        }
        _ => json_response(
            StatusCode::NOT_FOUND,
            json!({ "error": { "message": format!("missing mock route: {method} {uri}") } }),
        ),
    }
}

fn json_response(status: StatusCode, body: Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

async fn spawn_mock(mode: MockMode) -> (SocketAddr, Arc<Mutex<Vec<SeenRequest>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().fallback(mock_api).with_state(MockState {
        mode,
        seen: Arc::clone(&seen),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock");
    let address = listener.local_addr().expect("mock address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock server");
    });
    (address, seen)
}

fn remote_client(address: SocketAddr, timeout: Duration) -> DaemonClient {
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve("daemon.invalid", address)
        .build()
        .expect("test HTTP client");
    let config =
        ClientConfig::new_insecure_remote_http(format!("http://daemon.invalid:{}", address.port()))
            .expect("remote URL")
            .with_api_key(SecretString::new(CONTROL_KEY))
            .with_timeout(timeout);
    DaemonClient::with_http_client(config, http).expect("remote daemon client")
}

#[test]
fn endpoint_validation_rejects_invalid_urls_and_remote_without_an_api_key() {
    for invalid in [
        "ftp://daemon.example:3456",
        "http:///missing-host",
        "http://daemon.example:0",
        "http://daemon.example:65536",
        "http://user:password@daemon.example:3456",
        "http://daemon.example:3456?api_key=secret",
    ] {
        let error = ClientConfig::new(invalid).expect_err("URL must be rejected");
        assert_eq!(error.kind(), ClientErrorKind::InvalidEndpoint, "{invalid}");
    }

    let insecure = ClientConfig::new("http://daemon.example:3456")
        .expect_err("remote HTTP must require explicit insecure opt-in");
    assert_eq!(insecure.kind(), ClientErrorKind::InsecureEndpoint);

    ClientConfig::new_insecure_remote_http("http://daemon.example:3456")
        .expect("the narrowly named development/test opt-in is explicit");

    let config = ClientConfig::new("https://daemon.example:3456").expect("valid remote URL");
    let error = DaemonClient::new(config).expect_err("remote endpoint needs x-api-key");
    assert_eq!(error.kind(), ClientErrorKind::MissingApiKey);

    let ipv6_loopback = ClientConfig::new("http://[::1]:3456").expect("IPv6 loopback HTTP");
    assert!(!ipv6_loopback.is_remote());
}

#[tokio::test]
async fn production_http_client_does_not_follow_redirects() {
    let (sink_address, sink_seen) = spawn_mock(MockMode::Normal).await;
    let location = format!("http://127.0.0.1:{}/llmux/dashboard", sink_address.port());
    let (redirect_address, redirect_seen) =
        spawn_mock(MockMode::RedirectDashboard { location }).await;
    let config = ClientConfig::new(format!("http://127.0.0.1:{}", redirect_address.port()))
        .expect("loopback HTTP remains allowed");
    let client = DaemonClient::new(config).expect("client");

    let error = client
        .fetch_dashboard()
        .await
        .expect_err("redirect must surface instead of being followed");

    assert_eq!(error.kind(), ClientErrorKind::HttpStatus);
    assert_eq!(
        error.status(),
        Some(StatusCode::TEMPORARY_REDIRECT.as_u16())
    );
    assert_eq!(redirect_seen.lock().expect("redirect seen").len(), 1);
    assert!(sink_seen.lock().expect("sink seen").is_empty());
}

#[tokio::test]
async fn client_matches_every_daemon_route_body_and_remote_auth_contract() {
    let (address, seen) = spawn_mock(MockMode::Normal).await;
    let client = remote_client(address, Duration::from_secs(2));

    let dashboard = client.fetch_dashboard().await.expect("dashboard");
    assert_eq!(dashboard.version, "llmux 0.2.16 (preview fixture)");

    client
        .run_operation(&OperationRequest::AddAccount {
            name: Some("api-fixture".into()),
            api_key: SecretString::new(ACCOUNT_KEY),
        })
        .await
        .expect("add account");
    client
        .run_operation(&OperationRequest::RemoveAccount {
            account_id: "claude:alice@example.com".into(),
            confirmed: true,
        })
        .await
        .expect("remove account");
    client
        .run_operation(&OperationRequest::PauseAccount {
            account_id: "claude:alice@example.com".into(),
            paused: true,
        })
        .await
        .expect("pause account");
    client
        .run_operation(&OperationRequest::UpdateSettings {
            email_anonymous: true,
        })
        .await
        .expect("settings");
    client
        .run_operation(&OperationRequest::UpsertEvent {
            event: EventDraft {
                id: "launch".into(),
                from: "2026-07-14T09:00:00+09:00".into(),
                to: "202607151800".into(),
                content: "Launch window".into(),
            },
        })
        .await
        .expect("event upsert");
    client
        .run_operation(&OperationRequest::RemoveEvent {
            event_id: "launch".into(),
        })
        .await
        .expect("event remove");

    let start = client
        .start_login(Provider::Grok)
        .await
        .expect("login start");
    assert_eq!(start.state, "daemon-state-1");
    let status = client
        .login_status(&start.state)
        .await
        .expect("login status");
    assert!(matches!(
        status,
        LoginStatus::Pending {
            state,
            verification_uri: Some(uri),
            user_code: Some(code),
            ..
        } if state == "daemon-state-1" && uri == "https://x.ai/device" && code == "ABCD-EFGH"
    ));
    assert!(client.cancel_login(&start.state).await.expect("cancel"));

    let requests = seen.lock().expect("seen lock");
    let routes: Vec<_> = requests
        .iter()
        .map(|request| (request.method.clone(), request.uri.as_str()))
        .collect();
    assert_eq!(
        routes,
        [
            (Method::GET, "/llmux/dashboard"),
            (Method::POST, "/llmux/add-account"),
            (Method::POST, "/llmux/remove-account"),
            (Method::POST, "/llmux/pause-account"),
            (Method::POST, "/llmux/settings"),
            (Method::POST, "/llmux/events"),
            (Method::POST, "/llmux/events"),
            (Method::POST, "/llmux/login/start"),
            (Method::GET, "/llmux/login/status?state=daemon-state-1"),
            (Method::POST, "/llmux/login/cancel"),
        ]
    );
    assert!(requests
        .iter()
        .all(|request| request.api_key.as_deref() == Some(CONTROL_KEY)));
    assert_eq!(
        requests[1].body,
        json!({ "name": "api-fixture", "api_key": ACCOUNT_KEY })
    );
    assert_eq!(
        requests[2].body,
        json!({ "name": "claude:alice@example.com", "confirm": true })
    );
    assert_eq!(
        requests[3].body,
        json!({ "account": "claude:alice@example.com", "paused": true })
    );
    assert_eq!(requests[4].body, json!({ "email_anonymous": true }));
    assert_eq!(
        requests[5].body,
        json!({
            "id": "launch",
            "from": "2026-07-14T09:00:00+09:00",
            "to": "202607151800",
            "content": "Launch window"
        })
    );
    assert_eq!(requests[6].body, json!({ "remove": "launch" }));
    assert_eq!(requests[7].body, json!({ "provider": "grok" }));
    assert_eq!(requests[9].body, json!({ "state": "daemon-state-1" }));
}

#[tokio::test]
async fn loopback_is_exempt_and_never_sends_the_configured_control_key() {
    let (address, seen) = spawn_mock(MockMode::Normal).await;
    let config = ClientConfig::new(format!("http://127.0.0.1:{}", address.port()))
        .expect("loopback URL")
        .with_api_key(SecretString::new(CONTROL_KEY));
    let client = DaemonClient::new(config).expect("loopback client");

    client.fetch_dashboard().await.expect("dashboard");
    assert_eq!(seen.lock().expect("seen lock")[0].api_key, None);
}

#[tokio::test]
async fn timeout_and_non_success_bodies_become_sanitized_typed_errors() {
    let (slow_address, _) = spawn_mock(MockMode::SlowDashboard).await;
    let slow = remote_client(slow_address, Duration::from_millis(20));
    let timeout = slow.fetch_dashboard().await.expect_err("must time out");
    assert_eq!(timeout.kind(), ClientErrorKind::Timeout);
    assert_eq!(timeout.to_string(), "daemon request timed out");

    let (error_address, _) = spawn_mock(MockMode::SecretError).await;
    let failing = remote_client(error_address, Duration::from_secs(1));
    let error = failing
        .run_operation(&OperationRequest::UpdateSettings {
            email_anonymous: true,
        })
        .await
        .expect_err("401 must fail");
    assert_eq!(error.kind(), ClientErrorKind::HttpStatus);
    assert_eq!(error.status(), Some(401));
    assert_eq!(error.to_string(), "daemon returned HTTP 401");
    let rendered = format!("{error:?} {error}");
    for forbidden in [
        CONTROL_KEY,
        ACCOUNT_KEY,
        "alice@example.com",
        "hunter2-from-daemon",
        "authentication_error",
    ] {
        assert!(!rendered.contains(forbidden), "leaked {forbidden}");
    }
}

#[tokio::test]
async fn executor_preserves_ids_and_leaves_platform_io_at_the_effect_boundary() {
    let (address, seen) = spawn_mock(MockMode::Normal).await;
    let client = remote_client(address, Duration::from_secs(1));
    let execution = client
        .execute(
            Effect::UpdateSettings {
                operation_id: "settings-9".into(),
                email_anonymous: true,
            },
            90,
        )
        .await;
    assert!(matches!(
        execution,
        EffectExecution::Action(Action::OperationFinished {
            id,
            outcome: OperationOutcome::Succeeded,
            finished_at_ms,
            ..
        }) if id == "settings-9" && finished_at_ms > 90
    ));

    let platform = client
        .execute(
            Effect::RunMaintenance {
                operation_id: "maint-1".into(),
                command: MaintenanceCommand::ChangeChannel {
                    channel: ReleaseChannel::Preview,
                },
            },
            100,
        )
        .await;
    assert!(matches!(
        platform,
        EffectExecution::Platform(Effect::RunMaintenance { operation_id, .. })
            if operation_id == "maint-1"
    ));
    assert_eq!(seen.lock().expect("seen lock").len(), 1);
}

#[tokio::test]
async fn effect_result_timestamp_is_captured_after_the_await() {
    let (address, _) = spawn_mock(MockMode::SlowDashboard).await;
    let client = remote_client(address, Duration::from_secs(1));
    let before = epoch_ms();

    let execution = client
        .execute(
            Effect::FetchDashboard {
                request_id: "dashboard-timestamp".into(),
            },
            1,
        )
        .await;
    let after = epoch_ms();

    assert!(matches!(
        execution,
        EffectExecution::Action(Action::DashboardReceived {
            request_id,
            received_at_ms,
            ..
        }) if request_id == "dashboard-timestamp"
            && received_at_ms >= before
            && received_at_ms <= after
    ));
}

#[tokio::test]
async fn add_account_false_is_a_verified_no_change() {
    let (address, _) = spawn_mock(MockMode::AddAccountNoChange).await;
    let client = remote_client(address, Duration::from_secs(1));

    let ack = client
        .run_operation(&OperationRequest::AddAccount {
            name: Some("existing".into()),
            api_key: SecretString::new(ACCOUNT_KEY),
        })
        .await
        .expect("valid no-change response");

    assert_eq!(ack.outcome, OperationOutcome::NoChange);
    assert!(ack.message.contains("already"));
}

#[tokio::test]
async fn transient_login_poll_failures_remain_pending_for_bounded_platform_retry() {
    let (address, _) = spawn_mock(MockMode::TransientLoginFailure).await;
    let client = remote_client(address, Duration::from_secs(1));

    let execution = client
        .execute(
            Effect::PollLogin {
                operation_id: "login-transient".into(),
                state: "daemon-state-1".into(),
            },
            120,
        )
        .await;

    assert!(matches!(
        execution,
        EffectExecution::Action(Action::LoginStatusReceived {
            operation_id,
            status: LoginStatus::Pending {
                state,
                message: Some(message),
                ..
            },
            at_ms,
        }) if operation_id == "login-transient"
            && state == "daemon-state-1"
            && message.contains("retry")
            && at_ms > 120
    ));
}

#[tokio::test]
async fn successful_login_error_envelopes_never_promote_daemon_free_text() {
    let (address, _) = spawn_mock(MockMode::LoginBodyError).await;
    let client = remote_client(address, Duration::from_secs(1));

    let status = client
        .login_status("daemon-state-1")
        .await
        .expect("the typed 2xx envelope is valid");

    assert_eq!(
        status,
        LoginStatus::Failed {
            message: "login failed".into()
        }
    );
    let rendered = format!("{status:?}");
    for forbidden in [
        "retired-daemon-account-42",
        "retired@example.com",
        "hunter2",
    ] {
        assert!(!rendered.contains(forbidden), "leaked {forbidden}");
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}
