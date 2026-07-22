use std::time::Duration;

use llmux_islands_core::{
    Action, Effect, LocalSettingsChange, MaintenanceCommand, Navigation, OpenReason,
    OperationOutcome, OperationRequest, Presentation, Provider, RefreshSource, ReleaseChannel,
    VerificationOperation,
};
use llmux_islands_linux::{
    settings::{ApiKey, LocalSettings},
    ControllerConfig, ControllerModel, PlatformRequest, DASHBOARD_POLL_INTERVAL, DEFAULT_ENDPOINT,
    LOGIN_DEADLINE, LOGIN_POLL_INTERVAL,
};
use serde_json::Value;

fn model() -> ControllerModel {
    let config = ControllerConfig::from_values(DEFAULT_ENDPOINT, None, Presentation::Regular)
        .expect("loopback controller config");
    ControllerModel::from_fixture(config.derive_options(), 1_000)
        .expect("canonical DashboardDoc fixture")
}

#[test]
fn live_controller_constructor_starts_empty_until_a_real_dashboard_receipt_arrives() {
    let config = ControllerConfig::from_values(DEFAULT_ENDPOINT, None, Presentation::Regular)
        .expect("loopback controller config");
    let live = ControllerModel::new(config.derive_options());

    assert_eq!(
        live.ui_state().lifecycle,
        llmux_islands_core::Lifecycle::Starting
    );
    assert!(live.ui_state().usage.accounts.is_empty());
    assert!(live.ui_state().statistics.models.is_empty());
    assert!(live.ui_state().statistics.activity_receipts.is_empty());
}

fn dispatch(action: &str, payload: &str) -> llmux_islands_linux::DispatchOutcome {
    model().dispatch(action, payload, 2_000)
}

#[test]
fn fixture_is_a_dashboard_doc_projected_to_canonical_ui_state_without_secrets() {
    let secret = "ta-controller-secret-value";
    let config = ControllerConfig::from_values(
        "https://daemon.example:3456",
        Some(secret.to_string()),
        Presentation::Regular,
    )
    .expect("authenticated remote controller config");
    let model = ControllerModel::from_fixture(config.derive_options(), 1_000)
        .expect("canonical DashboardDoc fixture");
    let state_json = model.state_json().expect("serialize UiState");
    let state: Value = serde_json::from_str(&state_json).expect("UiState JSON");

    assert_eq!(state["lifecycle"], "ready");
    assert_eq!(state["navigation"], "usage");
    assert_eq!(
        state.pointer("/connection/endpoint_display"),
        Some(&Value::String("https://daemon.example:3456".into()))
    );
    assert_eq!(state["usage"]["accounts"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        state.pointer("/settings/maintenance/islands_version"),
        Some(&Value::String(env!("CARGO_PKG_VERSION").into()))
    );
    assert_eq!(
        state.pointer("/settings/maintenance/license"),
        Some(&Value::String("MIT".into()))
    );
    assert_eq!(
        state.pointer("/settings/maintenance/source_url"),
        Some(&Value::String("https://github.com/2lab-ai/llmux".into()))
    );
    assert_eq!(
        state.pointer("/verification_receipts/0/id"),
        Some(&Value::String("snapshot-settings-readback".into()))
    );
    assert_eq!(
        state.pointer("/verification_receipts/0/outcome"),
        Some(&Value::String("succeeded".into()))
    );
    assert!(state.get("selected_surface").is_none());
    assert!(!state_json.contains(secret));
    assert!(!format!("{config:?}").contains(secret));
}

#[test]
fn app_started_ensures_only_loopback_then_fetches_the_dashboard() {
    let mut local = model();
    assert!(matches!(
        local.apply(Action::AppStarted).as_slice(),
        [Effect::EnsureLocalDaemon, Effect::FetchDashboard { .. }]
    ));

    let remote_config = ControllerConfig::from_values(
        "https://daemon.example:3456",
        Some("ta-remote-control-secret".into()),
        Presentation::Regular,
    )
    .expect("remote config");
    let mut remote = ControllerModel::from_fixture(remote_config.derive_options(), 1_000)
        .expect("remote fixture model");
    assert!(matches!(
        remote.apply(Action::AppStarted).as_slice(),
        [Effect::FetchDashboard { .. }]
    ));
}

#[test]
fn semantic_navigation_refresh_and_login_actions_preserve_core_ids() {
    let mut navigation = model();
    let selected = navigation.dispatch(
        "navigation_selected",
        r#"{"navigation":"statistics"}"#,
        2_000,
    );
    assert!(selected.effects.is_empty());
    assert!(selected.platform.is_empty());
    assert_eq!(navigation.ui_state().navigation, Navigation::Statistics);

    assert!(matches!(
        dispatch("refresh_requested", r#"{"source":"manual"}"#)
            .effects
            .as_slice(),
        [Effect::FetchDashboard { .. }]
    ));

    let login = dispatch("login_started", r#"{"provider":"grok"}"#);
    assert!(matches!(
        login.effects.as_slice(),
        [Effect::StartLogin {
            operation_id,
            provider: Provider::Grok
        }] if operation_id.starts_with("login-")
    ));

    let mut cancel = model();
    let login = cancel.dispatch("login_started", r#"{"provider":"codex"}"#, 2_000);
    let operation_id = match login.effects.as_slice() {
        [Effect::StartLogin { operation_id, .. }] => operation_id.clone(),
        other => panic!("unexpected login effects: {other:?}"),
    };
    let pending = cancel.apply(Action::LoginStatusReceived {
        operation_id: operation_id.clone(),
        status: llmux_islands_core::LoginStatus::Pending {
            state: "daemon-state".into(),
            verification_uri: None,
            user_code: None,
            message: None,
        },
        at_ms: 2_010,
    });
    assert!(matches!(pending.as_slice(), [Effect::PollLogin { .. }]));
    assert!(
        !cancel
            .state_json()
            .expect("pending login state JSON")
            .contains("daemon-state"),
        "the daemon OAuth state must remain effect-private"
    );
    let cancellation = cancel.dispatch("login_cancelled", "{}", 2_020);
    assert!(matches!(
        cancellation.effects.as_slice(),
        [
            Effect::StopLoginPoll {
                operation_id: stopped_id
            },
            Effect::CancelLogin {
                operation_id: emitted_id,
                state
            }
        ] if stopped_id == &operation_id
            && emitted_id == &operation_id
            && state == "daemon-state"
    ));
    assert_eq!(
        cancel.ui_state().usage.login.phase,
        llmux_islands_core::LoginPhase::Cancelling
    );
}

#[test]
fn semantic_window_lifecycle_tracks_reason_metrics_and_close_state() {
    let mut model = model();
    assert!(!model.ui_state().window.open);

    let opened = model.dispatch("tray_activated", "{}", 2_000);
    assert!(opened.error.is_none());
    assert!(model.ui_state().window.open);
    assert_eq!(model.ui_state().window.open_reason, OpenReason::Click);

    let resized = model.dispatch(
        "window_metrics_changed",
        r#"{"width":900,"content_height":612}"#,
        2_001,
    );
    assert!(resized.error.is_none());
    assert_eq!(model.ui_state().window.width, 900);
    assert_eq!(model.ui_state().window.content_height, 612);

    let closed = model.dispatch("close_requested", "{}", 2_002);
    assert!(closed.error.is_none());
    assert!(!model.ui_state().window.open);
    assert_eq!(model.ui_state().window.open_reason, OpenReason::None);

    let notification = model.dispatch("open_requested", r#"{"reason":"notification"}"#, 2_003);
    assert!(notification.error.is_none());
    assert!(model.ui_state().window.open);
    assert_eq!(
        model.ui_state().window.open_reason,
        OpenReason::Notification
    );
}

#[test]
fn boot_close_elapsed_closes_only_the_untouched_tray_backed_boot_presentation() {
    let mut fallback = model();
    fallback.dispatch("open_requested", r#"{"reason":"boot"}"#, 2_000);
    let no_tray = fallback.dispatch("boot_close_elapsed", r#"{"tray_available":false}"#, 3_000);
    assert!(no_tray.error.is_none());
    assert!(fallback.ui_state().window.open);
    assert_eq!(fallback.ui_state().window.open_reason, OpenReason::Boot);

    let tray_backed = fallback.dispatch("boot_close_elapsed", r#"{"tray_available":true}"#, 3_001);
    assert!(tray_backed.error.is_none());
    assert!(!fallback.ui_state().window.open);
    assert_eq!(fallback.ui_state().window.open_reason, OpenReason::None);

    let mut interacted = model();
    interacted.dispatch("open_requested", r#"{"reason":"boot"}"#, 2_000);
    interacted.dispatch("open_requested", r#"{"reason":"click"}"#, 2_100);
    interacted.dispatch("boot_close_elapsed", r#"{"tray_available":true}"#, 3_000);
    assert!(interacted.ui_state().window.open);
    assert_eq!(interacted.ui_state().window.open_reason, OpenReason::Click);

    assert!(
        model()
            .dispatch("boot_close_elapsed", "{}", 3_000)
            .error
            .is_some(),
        "missing tray capability evidence must not close the window"
    );
}

#[test]
fn account_settings_event_autostart_and_maintenance_actions_are_typed() {
    let api_secret = "sk-account-secret-value";
    let mut add = model();
    let result = add.dispatch(
        "add_api_key_submitted",
        &format!(r#"{{"name":"api-prod","api_key":"{api_secret}"}}"#),
        3_000,
    );
    assert!(matches!(
        result.effects.as_slice(),
        [Effect::RunOperation {
            operation_id,
            request: OperationRequest::AddAccount { name, api_key }
        }] if operation_id.starts_with("add-account-")
            && name.as_deref() == Some("api-prod")
            && api_key.expose_secret() == api_secret
    ));
    assert!(!add.state_json().expect("state JSON").contains(api_secret));

    let mut pause = model();
    let pause_handle = pause
        .ui_state()
        .usage
        .accounts
        .iter()
        .find(|account| account.provider == Provider::Claude)
        .expect("Claude account handle")
        .id
        .clone();
    let paused = pause.dispatch(
        "pause_account_requested",
        &serde_json::json!({ "account": &pause_handle, "paused": true }).to_string(),
        3_010,
    );
    assert!(matches!(
        paused.effects.as_slice(),
        [Effect::RunOperation {
            request: OperationRequest::PauseAccount {
                account_id,
                paused: true
            },
            ..
        }] if account_id == "alice@example.com" && account_id != &pause_handle
    ));
    let mut remove = model();
    let remove_handle = remove
        .ui_state()
        .usage
        .accounts
        .iter()
        .find(|account| account.provider == Provider::Claude)
        .expect("Claude account handle")
        .id
        .clone();
    let removed = remove.dispatch(
        "remove_account_confirmed",
        &serde_json::json!({ "account": &remove_handle }).to_string(),
        3_020,
    );
    assert!(matches!(
        removed.effects.as_slice(),
        [Effect::RunOperation {
            request: OperationRequest::RemoveAccount {
                account_id,
                confirmed: true
            },
            ..
        }] if account_id == "alice@example.com" && account_id != &remove_handle
    ));
    assert!(matches!(
        dispatch("email_anonymous_changed", r#"{"enabled":true}"#)
            .effects
            .as_slice(),
        [Effect::UpdateSettings {
            email_anonymous: true,
            ..
        }]
    ));
    assert!(matches!(
        dispatch(
            "event_upsert_requested",
            r#"{"id":"launch","from":"2026-07-14T09:00:00+09:00","to":"2026-07-15T18:00:00+09:00","content":"Launch"}"#
        )
        .effects
        .as_slice(),
        [Effect::UpsertEvent { event, .. }] if event.id == "launch"
    ));
    assert!(matches!(
        dispatch("event_remove_requested", r#"{"id":"launch"}"#)
            .effects
            .as_slice(),
        [Effect::RemoveEvent { event_id, .. }] if event_id == "launch"
    ));
    assert!(matches!(
        dispatch("set_autostart", r#"{"enabled":true}"#)
            .effects
            .as_slice(),
        [Effect::SetAutostart { enabled: true, .. }]
    ));
    assert!(matches!(
        dispatch(
            "maintenance_requested",
            r#"{"kind":"change_channel","channel":"preview"}"#
        )
        .effects
        .as_slice(),
        [Effect::RunMaintenance {
            command: MaintenanceCommand::ChangeChannel {
                channel: ReleaseChannel::Preview
            },
            ..
        }]
    ));
}

#[test]
fn local_weekly_setting_completes_through_one_typed_receipt() {
    let mut model = model();
    let result = model.dispatch("show_fable_changed", r#"{"enabled":false}"#, 4_000);

    assert!(result.platform.is_empty());
    let operation_id = match result.effects.as_slice() {
        [Effect::PersistSettings {
            operation_id,
            change: LocalSettingsChange::ShowFable { enabled: false },
        }] => operation_id.clone(),
        other => panic!("unexpected local settings boundary: {other:?}"),
    };
    assert_eq!(
        model
            .ui_state()
            .operation
            .as_ref()
            .map(|operation| operation.kind.as_str()),
        Some("settings")
    );
    model.apply(Action::OperationFinished {
        id: operation_id,
        outcome: OperationOutcome::NoChange,
        message: "settings were already current".into(),
        finished_at_ms: 4_010,
    });
    let state = model.ui_state();
    assert!(state.operation.is_none());
    assert_eq!(
        state
            .verification_receipts
            .last()
            .map(|receipt| receipt.operation),
        Some(VerificationOperation::Settings)
    );
}

#[test]
fn direct_platform_actions_carry_only_the_minimum_typed_payload() {
    for (action, payload, expected) in [
        (
            "open_url_requested",
            r#"{"url":"https://x.ai/device"}"#,
            PlatformRequest::OpenUrl("https://x.ai/device".into()),
        ),
        (
            "copy_text_requested",
            r#"{"text":"ABCD-EFGH"}"#,
            PlatformRequest::CopyText("ABCD-EFGH".into()),
        ),
        (
            "test_notification",
            "{}",
            PlatformRequest::TestNotification { sound_id: None },
        ),
        ("quit_requested", "{}", PlatformRequest::Quit),
    ] {
        let result = dispatch(action, payload);
        assert_eq!(result.platform, [expected]);
        assert!(result.effects.is_empty());
    }
}

#[test]
fn dashboard_polling_is_single_flight_and_stale_results_do_not_mutate_state() {
    let mut model = model();
    let request_id = match model
        .apply(Action::RefreshRequested {
            source: RefreshSource::Manual,
        })
        .as_slice()
    {
        [Effect::FetchDashboard { request_id }] => request_id.clone(),
        other => panic!("unexpected refresh effects: {other:?}"),
    };
    assert!(model
        .apply(Action::RefreshRequested {
            source: RefreshSource::Poll,
        })
        .is_empty());

    let before = model.ui_state();
    assert!(model
        .apply(Action::DashboardFailed {
            request_id: "stale-dashboard".into(),
            error: "late failure".into(),
            failed_at_ms: 2_000,
        })
        .is_empty());
    assert_eq!(model.ui_state(), before);

    let retry = model.apply(Action::DashboardFailed {
        request_id,
        error: "offline".into(),
        failed_at_ms: 2_000,
    });
    assert!(matches!(
        retry.as_slice(),
        [Effect::ScheduleDashboardRetry { retry_at_ms: 3_000 }]
    ));
    assert!(model
        .apply(Action::RefreshRequested {
            source: RefreshSource::Poll,
        })
        .is_empty());
    assert!(matches!(
        model
            .apply(Action::RefreshRequested {
                source: RefreshSource::Retry,
            })
            .as_slice(),
        [Effect::FetchDashboard { .. }]
    ));
}

#[test]
fn polling_cadence_and_deadline_are_bounded() {
    assert_eq!(DASHBOARD_POLL_INTERVAL, Duration::from_secs(10));
    assert!(LOGIN_POLL_INTERVAL >= Duration::from_secs(1));
    assert!(LOGIN_POLL_INTERVAL <= Duration::from_secs(5));
    assert_eq!(LOGIN_DEADLINE, Duration::from_secs(5 * 60));
}

#[test]
fn qt_screen_inventory_is_projected_and_selection_stays_typed() {
    let mut model = model();
    let inventory = model.dispatch(
        "screen_inventory_changed",
        r#"{"screens":[{"id":"qt-screen:HDMI-A-1","label":"Dell · 2560×1440"},{"id":"qt-screen:DP-1","label":"LG · 1920×1080"}]}"#,
        4_500,
    );
    assert!(inventory.effects.is_empty());
    assert!(inventory.error.is_none());
    let screens = model.ui_state().settings.screens;
    assert_eq!(screens.len(), 3);
    assert_eq!(screens[0]["id"], "auto");
    assert_eq!(screens[1]["id"], "qt-screen:HDMI-A-1");

    let selected = model.dispatch("screen_selected", r#"{"id":"qt-screen:DP-1"}"#, 4_510);
    assert!(matches!(
        selected.effects.as_slice(),
        [Effect::PersistSettings {
            change: LocalSettingsChange::ScreenSelected { id },
            ..
        }] if id == "qt-screen:DP-1"
    ));
}

#[test]
fn connection_settings_build_valid_ipv4_and_bracketed_ipv6_endpoints() {
    for (payload, expected) in [
        (
            r#"{"host":"127.0.0.1","port":3456}"#,
            "http://127.0.0.1:3456",
        ),
        (
            r#"{"scheme":"https","host":"::1","port":8443,"api_key":"ta-secret"}"#,
            "https://[::1]:8443",
        ),
        (r#"{"host":"[::1]","port":3456}"#, "http://[::1]:3456"),
    ] {
        let mut model = model();
        let result = model.dispatch("connection_apply_requested", payload, 5_000);
        assert!(
            matches!(
                result.effects.as_slice(),
                [Effect::PersistSettings {
                    change: LocalSettingsChange::ConnectionApplied {
                        endpoint,
                        ..
                    },
                    ..
                }] if endpoint == expected
            ),
            "unexpected connection effects for {payload} -> {expected}: {:?}; error: {:?}",
            result.effects,
            result.error
        );
        let applied = ControllerConfig::from_values(expected, None, Presentation::Regular)
            .expect("applied connection config");
        model.set_connection_options(applied.derive_options());
        assert_eq!(model.ui_state().connection.endpoint_display, expected);
        assert!(!model
            .state_json()
            .expect("UiState JSON")
            .contains("ta-secret"));
    }
}

#[test]
fn connection_apply_without_a_new_key_preserves_the_configured_secret() {
    let secret = "ta-preserved-secret";
    let mut model = model();
    model.set_platform_state(
        LocalSettings {
            api_key: Some(ApiKey::new(secret)),
            ..LocalSettings::default()
        },
        false,
        "self-managed",
    );

    let result = model.dispatch(
        "connection_apply_requested",
        r#"{"scheme":"https","host":"daemon.example","port":8443}"#,
        6_000,
    );
    assert!(matches!(
        result.effects.as_slice(),
        [Effect::PersistSettings {
            change: LocalSettingsChange::ConnectionApplied {
                api_key: Some(api_key),
                ..
            },
            ..
        }] if api_key.expose_secret() == secret
    ));
    assert!(!model.state_json().expect("UiState JSON").contains(secret));
}

#[test]
fn connection_api_key_requires_an_explicit_keep_replace_or_clear_result() {
    let secret = "ta-cleared-secret";
    let mut configured = model();
    configured.set_platform_state(
        LocalSettings {
            api_key: Some(ApiKey::new(secret)),
            ..LocalSettings::default()
        },
        false,
        "self-managed",
    );

    let cleared = configured.dispatch(
        "connection_apply_requested",
        r#"{"scheme":"https","host":"daemon.example","port":8443,"api_key_mode":"clear"}"#,
        6_100,
    );
    assert!(matches!(
        cleared.effects.as_slice(),
        [Effect::PersistSettings {
            change: LocalSettingsChange::ConnectionApplied { api_key: None, .. },
            ..
        }]
    ));
    assert!(!configured
        .state_json()
        .expect("UiState JSON")
        .contains(secret));

    let mut unauthenticated = model();
    let implicit_missing = unauthenticated.dispatch(
        "connection_apply_requested",
        r#"{"scheme":"https","host":"daemon.example","port":8443,"api_key_mode":"keep"}"#,
        6_200,
    );
    assert!(implicit_missing.effects.is_empty());
    assert_eq!(
        implicit_missing.error.as_deref(),
        Some("remote connection requires an API key or an explicit clear")
    );
}

#[test]
fn desktop_capabilities_are_unknown_until_qt_reports_runtime_availability() {
    let mut model = model();
    let initial = model.ui_state();
    assert_eq!(
        initial.settings.capabilities["tray"]["available"],
        Value::Bool(false)
    );
    assert!(initial.settings.capabilities["tray"]["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("not been detected")));

    let detected = model.dispatch(
        "desktop_capabilities_changed",
        r#"{"tray_available":true,"notifications_available":true}"#,
        6_300,
    );
    assert!(detected.error.is_none());
    let state = model.ui_state();
    assert_eq!(
        state.settings.capabilities["tray"]["available"],
        Value::Bool(true)
    );
    assert_eq!(
        state.settings.capabilities["notifications"]["available"],
        Value::Bool(true)
    );
}
