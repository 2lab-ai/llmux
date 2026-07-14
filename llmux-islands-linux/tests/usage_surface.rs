use std::{fs, path::PathBuf};

fn crate_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read(relative: &str) -> String {
    fs::read_to_string(crate_path(relative))
        .unwrap_or_else(|error| panic!("missing usage contract resource {relative}: {error}"))
}

#[test]
fn renderer_consumes_the_canonical_usage_settings_and_receipt_contract() {
    let qml = read("qml/Usage.qml");

    for field in [
        "uiState.usage",
        "uiState.settings",
        "uiState.connection",
        "uiState.verification_receipts",
        "usage.accounts",
        "usage.current_by_group",
        "usage.provider_in_flight",
        "usage.login",
        "settings.show_fable_weekly",
    ] {
        assert!(qml.contains(field), "renderer must consume {field}");
    }

    for legacy_field in [
        "account.account",
        "account.tier",
        "account.five_hour",
        "account.seven_day",
        "usageState",
    ] {
        assert!(
            !qml.contains(legacy_field),
            "renderer must not depend on POC field {legacy_field}"
        );
    }
}

#[test]
fn offline_empty_and_additive_contract_values_are_tolerated() {
    let qml = read("qml/Usage.qml");

    for helper in [
        "function arrayOrEmpty",
        "function objectOrEmpty",
        "function hasValue",
        "function hasOwn",
        "function clampFraction",
        "function optionalText",
    ] {
        assert!(qml.contains(helper), "missing tolerant helper {helper}");
    }

    for surface in ["usage-offline-state", "usage-empty-state"] {
        assert!(
            qml.contains(&format!("objectName: \"{surface}\"")),
            "missing state surface {surface}"
        );
    }

    assert!(qml.contains("uiState.lifecycle"));
    assert!(qml.contains("connection.error"));
    assert!(qml.contains("qsTr(\"Unavailable\")"));
    assert!(
        !qml.contains("|| 0"),
        "missing telemetry must not silently become a misleading zero"
    );
}

#[test]
fn account_tiles_render_only_the_semantic_privacy_safe_inventory() {
    let qml = read("qml/Usage.qml");

    for marker in [
        "account.display_name",
        "account.provider",
        "account.current",
        "account.paused",
        "account.healthy",
        "account.status",
        "account.blocked_reason",
        "account.in_flight",
        "account.token_expiry",
        "tokenExpiry.state",
        "tokenExpiry.countdown_text",
        "tokenExpiry.expires_at_ms",
        "account.warning_level",
        "account.busy_action",
    ] {
        assert!(
            qml.contains(marker),
            "missing account renderer marker {marker}"
        );
    }

    for forbidden in [
        "account.email",
        "account.name",
        "account.access_token",
        "account.refresh_token",
        "account.authorization",
        "account.provider_token",
    ] {
        assert!(
            !qml.to_ascii_lowercase().contains(forbidden),
            "tile renderer must never reference secret/raw field {forbidden}"
        );
    }
}

#[test]
fn gauges_are_dynamic_and_cover_five_hour_seven_day_and_fable() {
    let qml = read("qml/Usage.qml");

    for marker in [
        "account.gauges",
        "gauge.kind",
        "gauge.available",
        "gauge.used_fraction",
        "gauge.remaining_fraction",
        "gauge.resets_at",
        "gauge.reset_text",
        "gauge.constraining",
        "five_hour",
        "seven_day",
        "fable_weekly",
        "settings.show_fable_weekly",
    ] {
        assert!(qml.contains(marker), "missing gauge marker {marker}");
    }

    assert!(
        qml.contains("Repeater") && qml.contains("visibleGauges"),
        "gauge rows must be rendered from the additive gauge array"
    );
}

#[test]
fn account_actions_are_semantic_scoped_and_remove_is_confirmation_gated() {
    let qml = read("qml/Usage.qml");

    assert!(qml.contains("signal dispatchRequested(string action, string payloadJson)"));
    for action in [
        "refresh_requested",
        "pause_account_requested",
        "remove_account_confirmed",
        "login_started",
        "login_cancelled",
        "add_api_key_submitted",
        "open_url_requested",
        "copy_text_requested",
    ] {
        assert!(
            qml.contains(&format!("\"{action}\"")),
            "missing action {action}"
        );
    }

    assert!(qml.contains("objectName: \"remove-account-confirmation\""));
    assert!(qml.contains("onAccepted:"));
    assert!(qml.contains("login.phase === \"cancelling\""));
    assert!(qml.contains("!usagePage.loginCancelling"));
    assert!(
        !qml.contains("\"remove_account_requested\""),
        "removal must not dispatch before confirmation"
    );
}

#[test]
fn add_and_device_login_surfaces_do_not_retain_or_echo_secrets() {
    let qml = read("qml/Usage.qml");

    for provider in ["claude", "codex", "grok", "api"] {
        assert!(qml.contains(&format!("\"key\": \"{provider}\"")));
    }
    for field in [
        "login.phase",
        "login.provider",
        "login.verification_uri",
        "login.user_code",
        "login.message",
    ] {
        assert!(qml.contains(field), "missing login field {field}");
    }
    for terminal in ["done", "error", "cancelled"] {
        assert!(qml.contains(&format!("\"{terminal}\"")));
    }

    assert!(qml.contains("echoMode: TextInput.Password"));
    assert!(qml.contains("inputMethodHints: Qt.ImhSensitiveData | Qt.ImhNoPredictiveText"));
    assert_eq!(
        qml.matches("apiKeyField.text").count(),
        2,
        "the API key may only be read into the dispatch payload and immediately cleared"
    );
    assert!(qml.contains("apiKeyField.text = \"\""));
    assert!(qml.contains("login.phase === \"pending\""));
    assert!(qml.contains("\"login_cancelled\","));
    assert!(qml.contains("\"{}\""));
    assert!(
        !qml.contains("usagePage.login.state"),
        "the renderer must not read or round-trip daemon OAuth state"
    );
    for forbidden in [
        "property string apiKey",
        "property var apiKey",
        "text: apiKeyField.text",
        "console.",
        "request_body",
        "response_body",
        "authorization",
        "provider_token",
        "XMLHttpRequest",
        "WebSocket",
        "QtNetwork",
        "fetch(",
    ] {
        assert!(
            !qml.contains(forbidden),
            "usage renderer must not retain/render/log secret material via {forbidden}"
        );
    }
}

#[test]
fn terminal_usage_operations_have_a_verification_receipt_region() {
    let qml = read("qml/Usage.qml");

    assert!(qml.contains("objectName: \"usage-verification-receipts\""));
    for operation in ["login", "add_account", "pause_account", "remove_account"] {
        assert!(qml.contains(&format!("\"{operation}\"")));
    }
    for field in [
        "receipt.operation",
        "receipt.target_display",
        "receipt.started_at_ms",
        "receipt.finished_at_ms",
        "receipt.outcome",
        "receipt.message",
    ] {
        assert!(
            qml.contains(field),
            "missing verification receipt field {field}"
        );
    }
}

#[test]
fn test_contract_tracks_the_checked_in_schema_names() {
    let schema = read("../llmux-islands-core/contract/ui-contract.schema.json");
    for field in [
        "accounts",
        "current_by_group",
        "provider_in_flight",
        "login",
        "display_name",
        "token_expiry",
        "gauges",
        "warning_level",
        "busy_action",
        "show_fable_weekly",
        "verification_receipts",
    ] {
        assert!(
            schema.contains(&format!("\"{field}\"")),
            "schema must still expose {field}"
        );
    }
}
