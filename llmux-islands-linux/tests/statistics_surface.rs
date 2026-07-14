use std::{fs, path::PathBuf};

fn crate_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read(relative: &str) -> String {
    fs::read_to_string(crate_path(relative))
        .unwrap_or_else(|error| panic!("missing statistics contract resource {relative}: {error}"))
}

#[test]
fn renderer_consumes_the_canonical_statistics_contract() {
    let qml = read("qml/Statistics.qml");

    for field in [
        "uiState.statistics",
        "statistics.overview",
        "statistics.models",
        "statistics.clients",
        "statistics.health",
        "statistics.heatmaps",
        "statistics.activity_receipts",
        "uiState.verification_receipts",
        "statistics.data_quality",
    ] {
        assert!(qml.contains(field), "renderer must consume {field}");
    }

    for legacy_field in ["statistics.summary", "statistics.receipts"] {
        assert!(
            !qml.contains(legacy_field),
            "renderer must not depend on POC field {legacy_field}"
        );
    }
}

#[test]
fn every_statistics_surface_has_an_explicit_renderer() {
    let qml = read("qml/Statistics.qml");

    for object_name in [
        "statistics-overview",
        "statistics-heatmaps",
        "statistics-models",
        "statistics-clients",
        "statistics-health",
        "statistics-activity-receipts",
    ] {
        assert!(
            qml.contains(&format!("objectName: \"{object_name}\"")),
            "missing surface {object_name}"
        );
    }

    for quality_key in ["model_usage", "windowed", "cost", "cache"] {
        assert!(qml.contains(quality_key), "missing qualifier {quality_key}");
    }

    for marker in [
        "property alias snapshotReceiptTarget: receiptEvidenceSection",
        "function renderedVerificationReceiptCount()",
        "Verification receipts",
    ] {
        assert!(
            qml.contains(marker),
            "missing receipt evidence marker {marker}"
        );
    }
}

#[test]
fn overview_heatmap_model_client_and_health_rows_cover_the_inventory() {
    let qml = read("qml/Statistics.qml");

    for marker in [
        "overview.requests",
        "overview.tokens_in",
        "overview.tokens_out",
        "overview.errors",
        "overview.cost_usd",
        "models.slice(0, 3)",
        "[\"24h\", \"72h\"]",
        "heatmap.cells",
        "heatCell.modelData.account_display",
        "heatCell.modelData.requests",
        "heatCell.modelData.errors",
        "modelCard.modelData.requests",
        "modelCard.modelData.ok",
        "modelCard.modelData.errors",
        "modelCard.modelData.in_flight",
        "modelCard.modelData.last_used_ms",
        "modelCard.modelData.cache_read",
        "modelCard.modelData.cache_creation",
        "modelCard.modelData.cost_usd",
        "modelData.accounts",
        "clientCard.modelData.client",
        "clientCard.modelData.requests",
        "clientCard.modelData.errors",
        "clientCard.modelData.cost_usd",
        "clientCard.modelData.last_seen_ms",
        "credential_type",
        "cooldown_until_ms",
        "blocked_reason",
        "token_expires_at_ms",
        "last_refresh_ms",
    ] {
        assert!(
            qml.contains(marker),
            "missing inventory renderer marker {marker}"
        );
    }
}

#[test]
fn request_receipts_cover_all_metadata_without_rendering_bodies_or_secrets() {
    let qml = read("qml/Statistics.qml");

    for field in [
        "receipt_id",
        "kind",
        "occurred_at_ms",
        "status",
        "method",
        "path",
        "account_display",
        "provider",
        "model",
        "effort",
        "fast",
        "tokens.input",
        "tokens.output",
        "cache.read",
        "cache.creation",
        "cost_usd",
        "duration_ms",
        "elapsed_ms",
        "message",
        "error",
    ] {
        assert!(qml.contains(field), "receipt renderer must consume {field}");
    }

    for forbidden in [
        "request_body",
        "response_body",
        "prompt_content",
        "authorization",
        "api_key",
        "provider_token",
    ] {
        assert!(
            !qml.to_ascii_lowercase().contains(forbidden),
            "renderer must never reference secret-bearing field {forbidden}"
        );
    }
}

#[test]
fn optional_and_additive_values_render_as_unavailable_instead_of_zero() {
    let qml = read("qml/Statistics.qml");

    for helper in [
        "function arrayOrEmpty",
        "function objectOrEmpty",
        "function hasValue",
        "function optionalNumber",
        "function optionalTime",
    ] {
        assert!(qml.contains(helper), "missing tolerant helper {helper}");
    }

    assert!(
        qml.contains("qsTr(\"Unavailable\")"),
        "missing values need an explicit unavailable label"
    );
    assert!(
        !qml.contains("|| 0"),
        "optional telemetry must not silently become a misleading zero"
    );
}

#[test]
fn test_contract_tracks_the_checked_in_schema_names() {
    let schema = read("../llmux-islands-core/contract/ui-contract.schema.json");
    for field in [
        "overview",
        "models",
        "clients",
        "health",
        "heatmaps",
        "activity_receipts",
        "data_quality",
    ] {
        assert!(
            schema.contains(&format!("\"{field}\"")),
            "schema must still expose {field}"
        );
    }
}
