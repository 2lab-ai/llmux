use llmux::dashboard::DashboardDoc;
use llmux_islands_core::{derive_ui_state, DeriveOptions, GaugeKind, Presentation, Provider};
use std::collections::BTreeSet;

fn read_fixture(name: &str) -> DashboardDoc {
    let raw = match name {
        "current" => include_str!("../fixtures/dashboard-current.json"),
        "legacy" => include_str!("../fixtures/dashboard-legacy.json"),
        other => panic!("unknown fixture {other}"),
    };
    serde_json::from_str(raw).expect("fixture must deserialize as the root llmux DashboardDoc")
}

fn options() -> DeriveOptions {
    DeriveOptions {
        endpoint_display: "http://127.0.0.1:3456".into(),
        remote: false,
        authenticated: true,
        api_key_configured: true,
        selected_screen_id: "primary".into(),
        presentation: Presentation::LayerShell,
    }
}

#[test]
fn current_fixture_derives_accounts_gauges_and_activity_without_reordering() {
    let mut doc = read_fixture("current");
    doc.accounts[0].cooldown_until = Some(2_000_000_123);
    doc.accounts[0].cooldown_source = Some("retry_after".into());
    let state = derive_ui_state(&doc, &options(), 1_700_000_003_000);

    assert_eq!(state.usage.accounts.len(), 2);
    assert_eq!(state.usage.accounts[0].display_name, "a***@example.com");
    assert_eq!(state.usage.accounts[0].provider, Provider::Claude);
    assert!(state.usage.accounts[0].current);
    assert_eq!(state.usage.accounts[0].gauges.len(), 3);
    assert_eq!(
        state.usage.accounts[0].gauges[0].resets_at,
        Some(2_000_000_000_000),
        "semantic UI timestamps use epoch milliseconds"
    );
    assert_eq!(state.window.provider_in_flight.get("claude"), Some(&2));
    assert_eq!(state.window.provider_in_flight.get("codex"), Some(&1));
    assert_eq!(state.statistics.health[0]["kind"], "oauth");
    assert_eq!(
        state.statistics.health[0]["cooldown_until_ms"],
        2_000_000_123_000_u64
    );
    assert_eq!(state.statistics.health[0]["cooldown_source"], "retry_after");

    let receipts = &state.statistics.activity_receipts;
    assert_eq!(receipts.len(), 3);
    assert_eq!(receipts[0].receipt_id, "in_flight:99");
    assert_eq!(receipts[1].status, Some(200));
    assert!(receipts[2].error);
    assert_eq!(receipts[0].path.as_deref(), Some("/v1/messages"));
    assert_eq!(receipts[1].path.as_deref(), Some("/v1/messages"));
    assert_eq!(
        receipts[2].message.as_deref(),
        Some("account account-1 failed with HTTP 503")
    );
    assert!(!serde_json::to_string(receipts)
        .expect("activity receipts JSON")
        .contains("alice@example.com"));
}

#[test]
fn fable_visibility_does_not_remove_the_semantic_gauge_data() {
    let mut doc = read_fixture("current");
    doc.show_fable_weekly = false;

    let state = derive_ui_state(&doc, &options(), 1_700_000_003_000);
    let gauge = state.usage.accounts[0]
        .gauges
        .iter()
        .find(|gauge| gauge.kind == GaugeKind::FableWeekly)
        .expect("Fable gauge remains available to platform renderers");

    assert!(!state.settings.show_fable_weekly);
    assert!(gauge.available);
    assert_eq!(gauge.used_fraction, 0.97);
    assert!(gauge.constraining);
}

#[test]
fn anonymous_activity_notes_replace_non_email_account_ids_with_opaque_handles() {
    let mut doc = read_fixture("current");
    let raw_account_id = "daemon-account-id-42";
    doc.accounts[0].name = raw_account_id.to_string();
    let note = doc
        .activity
        .completed
        .iter_mut()
        .find_map(|row| match row {
            llmux::dashboard::CompletedDoc::Note { text, .. } => Some(text),
            llmux::dashboard::CompletedDoc::Request { .. } => None,
        })
        .expect("activity note fixture");
    *note = format!("account {raw_account_id} failed with HTTP 429");

    let state = derive_ui_state(&doc, &options(), 1_700_000_003_000);
    let note = state
        .statistics
        .activity_receipts
        .iter()
        .find(|receipt| receipt.message.is_some())
        .expect("projected note");

    assert_eq!(
        note.message.as_deref(),
        Some("account account-1 failed with HTTP 429")
    );
    assert!(!serde_json::to_string(&state)
        .expect("anonymous state JSON")
        .contains(raw_account_id));
}

#[test]
fn anonymous_activity_notes_mask_unseen_daemon_account_slots_without_flat_redaction() {
    let mut doc = read_fixture("current");
    let retired = "retired-daemon-account-42";
    let note = doc
        .activity
        .completed
        .iter_mut()
        .find_map(|row| match row {
            llmux::dashboard::CompletedDoc::Note { text, .. } => Some(text),
            llmux::dashboard::CompletedDoc::Request { .. } => None,
        })
        .expect("activity note fixture");
    *note = format!("switch {retired} → alice@example.com (manual)");
    doc.activity
        .completed
        .push(llmux::dashboard::CompletedDoc::Note {
            at_ms: 1_700_000_003_000,
            text: format!("token refreshed: {retired} (expires 1h 5m)"),
            error: false,
        });
    doc.activity
        .completed
        .push(llmux::dashboard::CompletedDoc::Note {
            at_ms: 1_700_000_004_000,
            text: format!(
                "upstream: 429 from {retired}: provider echoed another-retired-id · retry-after 30s"
            ),
            error: true,
        });
    doc.activity
        .completed
        .push(llmux::dashboard::CompletedDoc::Note {
            at_ms: 1_700_000_005_000,
            text: "history loaded: 4 persisted requests resumed".into(),
            error: false,
        });

    let state = derive_ui_state(&doc, &options(), 1_700_000_006_000);
    let messages: Vec<_> = state
        .statistics
        .activity_receipts
        .iter()
        .filter_map(|receipt| receipt.message.as_deref())
        .collect();

    assert!(messages.contains(&"switch anonymous → account-1 (manual)"));
    assert!(messages.contains(&"token refreshed: anonymous (expires 1h 5m)"));
    assert!(messages.contains(&"upstream: 429 from anonymous · retry-after 30s"));
    assert!(messages.contains(&"history loaded: 4 persisted requests resumed"));
    let serialized = serde_json::to_string(&state).expect("anonymous state JSON");
    assert!(!serialized.contains(retired));
    assert!(!serialized.contains("another-retired-id"));
}

#[test]
fn legacy_fixture_uses_additive_defaults_instead_of_failing_or_faking_data() {
    let doc = read_fixture("legacy");
    assert!(doc.model_usage.is_empty());
    assert!(doc.client_usage.is_empty());
    assert!(doc.windowed.is_empty());
    assert!(doc.current_by_group.is_empty());

    let state = derive_ui_state(&doc, &options(), 1_700_000_003_000);
    assert!(state.usage.accounts.is_empty());
    assert!(state.statistics.models.is_empty());
    assert!(state.statistics.heatmaps.is_empty());
    assert_eq!(
        state.statistics.data_quality["cache"],
        "missing fields shown as unavailable"
    );
}

#[test]
fn identical_completed_requests_receive_distinct_stable_receipt_ids() {
    let mut doc = read_fixture("current");
    let duplicate = doc.activity.completed[0].clone();
    doc.activity.completed.insert(1, duplicate);

    let state = derive_ui_state(&doc, &options(), 1_700_000_003_000);
    let ids: Vec<_> = state
        .statistics
        .activity_receipts
        .iter()
        .map(|receipt| receipt.receipt_id.as_str())
        .collect();
    let unique: BTreeSet<_> = ids.iter().copied().collect();

    assert_eq!(unique.len(), ids.len());
}
