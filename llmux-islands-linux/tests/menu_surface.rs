use std::{fs, path::PathBuf};

fn crate_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read(relative: &str) -> String {
    fs::read_to_string(crate_path(relative))
        .unwrap_or_else(|error| panic!("missing menu contract resource {relative}: {error}"))
}

#[test]
fn renderer_consumes_the_canonical_menu_contract() {
    let qml = read("qml/Menu.qml");

    for field in [
        "uiState.settings",
        "uiState.connection",
        "uiState.verification_receipts",
        "uiState.operation",
        "settings.screens",
        "settings.sounds",
        "settings.events",
        "settings.autostart",
        "settings.maintenance",
        "settings.capabilities",
        "connection.endpoint_display",
        "settings.api_key_configured",
    ] {
        assert!(qml.contains(field), "renderer must consume {field}");
    }

    for legacy_field in ["uiState.menu", "menuState"] {
        assert!(
            !qml.contains(legacy_field),
            "renderer must not depend on POC field {legacy_field}"
        );
    }
}

#[test]
fn additive_values_are_tolerated_and_render_as_unavailable() {
    let qml = read("qml/Menu.qml");

    for helper in [
        "function arrayOrEmpty",
        "function objectOrEmpty",
        "function hasOwn",
        "function optionalText",
        "function optionId",
        "function optionLabel",
    ] {
        assert!(qml.contains(helper), "missing tolerant helper {helper}");
    }

    assert!(qml.contains("qsTr(\"Unavailable\")"));
    assert!(
        !qml.contains("|| 0"),
        "missing additive values must not silently become a misleading zero"
    );
}

#[test]
fn settings_and_platform_controls_dispatch_only_semantic_actions() {
    let qml = read("qml/Menu.qml");

    assert_eq!(
        qml.matches("signal dispatchRequested(string action, string payloadJson)")
            .count(),
        1,
        "the renderer must expose one semantic dispatch boundary"
    );

    for action in [
        "screen_selected",
        "sound_selected",
        "sound_preview_requested",
        "email_anonymous_changed",
        "show_fable_changed",
        "connection_apply_requested",
        "event_upsert_requested",
        "event_remove_requested",
        "autostart_changed",
        "update_requested",
        "channel_change_requested",
        "open_url_requested",
        "quit_requested",
        "test_notification",
    ] {
        assert!(
            qml.contains(&format!("\"{action}\"")),
            "missing action {action}"
        );
    }

    for legacy_action in ["set_setting", "set_autostart"] {
        assert!(
            !qml.contains(&format!("\"{legacy_action}\"")),
            "renderer must not dispatch POC action {legacy_action}"
        );
    }

    for bypass in ["Qt.openUrlExternally", "Qt.quit()", "successMessage"] {
        assert!(
            !qml.contains(bypass),
            "platform effects and success claims must return through reducer state: {bypass}"
        );
    }
}

#[test]
fn connection_draft_validates_the_endpoint_and_never_echoes_the_api_key() {
    let qml = read("qml/Menu.qml");

    for marker in [
        "objectName: \"connection-settings\"",
        "objectName: \"connection-scheme-selector\"",
        "function validateConnectionDraft",
        "function endpointScheme",
        "function isLoopbackHost",
        "\"scheme\": connectionSchemeDraft",
        "Remote daemons require HTTPS",
        "model: [\"http\", \"https\"]",
        "portValue < 1 || portValue > 65535",
        "echoMode: TextInput.Password",
        "Qt.ImhSensitiveData | Qt.ImhNoPredictiveText",
        "settings.api_key_configured",
        "payload.api_key_mode = \"clear\"",
        "payload.api_key_mode = \"replace\"",
        "payload.api_key_mode = \"keep\"",
        "Clear the stored API key",
        "connectionApiKeyField.text = \"\"",
    ] {
        assert!(
            qml.contains(marker),
            "missing connection safety marker {marker}"
        );
    }

    assert!(
        !qml.contains("text: connectionApiKeyField.text"),
        "the API key must never be echoed by a text binding"
    );
    assert_eq!(
        qml.matches("connectionApiKeyField.text").count(),
        3,
        "the API key field may only be read for dispatch or cleared on explicit replacement/deletion"
    );
    for forbidden in ["console.", "authorization", "provider_token"] {
        assert!(
            !qml.to_ascii_lowercase().contains(forbidden),
            "the menu must not log or render secret material via {forbidden}"
        );
    }
}

#[test]
fn events_support_create_edit_validation_and_confirmed_removal() {
    let qml = read("qml/Menu.qml");

    for marker in [
        "objectName: \"event-editor\"",
        "objectName: \"event-remove-confirmation\"",
        "function validateEventDraft",
        "function parseCompactTimestamp",
        "function parseRfc3339Timestamp",
        "Event ID is required",
        "Event content is required",
        "The start must be earlier than the end",
        "editingExisting",
        "openForCreate",
        "openForEdit",
        "onAccepted:",
    ] {
        assert!(
            qml.contains(marker),
            "missing event workflow marker {marker}"
        );
    }

    assert!(
        !qml.contains("standardButtons: Dialog.Ok | Dialog.Cancel"),
        "the editor must validate before closing through an automatic OK button"
    );

    let removal_confirmation = qml
        .split("objectName: \"event-remove-confirmation\"")
        .nth(1)
        .expect("event removal confirmation body");
    assert!(removal_confirmation.contains("onAccepted:"));
    assert!(removal_confirmation.contains("\"event_remove_requested\""));
}

#[test]
fn maintenance_channel_changes_are_confirmation_gated() {
    let qml = read("qml/Menu.qml");

    for marker in [
        "objectName: \"maintenance-settings\"",
        "objectName: \"channel-change-confirmation\"",
        "pendingChannel",
        "currentChannel",
        "install_owner",
        "update_available",
        "latest_version",
    ] {
        assert!(qml.contains(marker), "missing maintenance marker {marker}");
    }

    let confirmation = qml
        .split("objectName: \"channel-change-confirmation\"")
        .nth(1)
        .expect("channel confirmation body");
    assert!(confirmation.contains("onAccepted:"));
    assert!(confirmation.contains("\"channel_change_requested\""));
}

#[test]
fn kde_capabilities_about_and_external_actions_are_explicit() {
    let qml = read("qml/Menu.qml");

    for marker in [
        "objectName: \"desktop-capabilities\"",
        "objectName: \"about-llmux-islands\"",
        "surfaceModeText",
        "capabilityExplanation",
        "layer_shell",
        "tray",
        "daemon_version",
        "license",
        "source_url",
        "aboutIslandsVersion",
        "aboutDaemonVersion",
        "aboutLicense",
        "aboutSourceUrl",
        "aboutReleasesUrl",
        "objectName: \"about-islands-version\"",
        "objectName: \"about-daemon-version\"",
        "objectName: \"about-license\"",
        "objectName: \"about-source-url\"",
        "Test notification",
        "objectName: \"open-releases\"",
        "qsTr(\"Releases\")",
        "objectName: \"accessibility-capability\"",
        "qsTr(\"Accessibility\")",
        "Not required on Plasma; no global pointer monitoring",
        "Quit Islands",
    ] {
        assert!(
            qml.contains(marker),
            "missing desktop/about marker {marker}"
        );
    }
}

#[test]
fn terminal_menu_operations_have_a_visible_verification_receipt_region() {
    let qml = read("qml/Menu.qml");

    assert!(qml.contains("objectName: \"menu-verification-receipts\""));
    for operation in ["settings", "event", "maintenance", "autostart"] {
        assert!(qml.contains(&format!("\"{operation}\"")));
    }
    for field in [
        "receipt.operation",
        "receipt.target_display",
        "receipt.started_at_ms",
        "receipt.finished_at_ms",
        "receipt.outcome",
        "receipt.message",
        "operation.kind",
        "operation.target_display",
        "operation.started_at_ms",
    ] {
        assert!(
            qml.contains(field),
            "missing operation/receipt field {field}"
        );
    }
}
