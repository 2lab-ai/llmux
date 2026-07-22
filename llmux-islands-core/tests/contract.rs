use llmux::dashboard::DashboardDoc;
use llmux_islands_core::{
    derive_ui_state, Action, Core, DeriveOptions, Effect, OperationRequest, Presentation,
    RefreshSource, SecretString,
};
use serde_json::Value;

fn state_json() -> Value {
    let doc: DashboardDoc =
        serde_json::from_str(include_str!("../fixtures/dashboard-current.json")).expect("fixture");
    let options = DeriveOptions {
        endpoint_display: "http://user:must-not-leak@127.0.0.1:3456?api_key=hidden".into(),
        remote: false,
        authenticated: true,
        api_key_configured: true,
        selected_screen_id: "primary".into(),
        presentation: Presentation::LayerShell,
    };
    serde_json::to_value(derive_ui_state(&doc, &options, 1_700_000_003_000))
        .expect("UiState must serialize")
}

#[test]
fn ui_state_serialization_validates_against_the_checked_in_schema() {
    let schema: Value = serde_json::from_str(include_str!("../contract/ui-contract.schema.json"))
        .expect("schema JSON");
    let state = state_json();
    let mut errors = Vec::new();
    validate(&schema, &schema, &state, "$", &mut errors);
    assert!(errors.is_empty(), "schema errors:\n{}", errors.join("\n"));
    assert!(
        state["settings"].get("sound_id").is_some() && state["settings"]["sound_id"].is_null(),
        "the common state reserves a typed platform sound selection"
    );
}

#[test]
fn initial_ui_state_validates_before_the_first_dashboard_response() {
    let schema: Value = serde_json::from_str(include_str!("../contract/ui-contract.schema.json"))
        .expect("schema JSON");
    let core = Core::new(DeriveOptions::default());
    let state = serde_json::to_value(core.state()).expect("initial state JSON");
    let mut errors = Vec::new();
    validate(&schema, &schema, &state, "$", &mut errors);
    assert!(errors.is_empty(), "schema errors:\n{}", errors.join("\n"));
    assert_eq!(state["statistics"]["overview"]["requests"], 0);
    assert_eq!(state["statistics"]["overview"]["cost_usd"], 0.0);
}

#[test]
fn state_and_debug_output_never_expose_api_keys_or_bearer_tokens() {
    let state = state_json();
    let serialized = serde_json::to_string(&state).expect("state JSON");
    for forbidden in [
        "must-not-leak",
        "Bearer-secret",
        "sk-secret-value",
        "api_key=hidden",
    ] {
        assert!(!serialized.contains(forbidden), "leaked {forbidden}");
    }

    let secret = "sk-live-super-secret-value";
    let mut core = Core::new(DeriveOptions::default());
    let effects = core.reduce(Action::OperationStarted {
        id: "op-secret".into(),
        request: OperationRequest::AddAccount {
            name: Some("fixture".into()),
            api_key: SecretString::new(secret),
        },
        target_display: Some("fixture".into()),
        started_at_ms: 10,
    });
    assert!(matches!(effects.as_slice(), [Effect::RunOperation { .. }]));
    assert!(!format!("{effects:?}").contains(secret));
    assert!(!format!("{:?}", core.state()).contains(secret));
}

#[test]
fn anonymous_ui_state_contains_only_opaque_account_handles() {
    let state = state_json();
    let serialized = serde_json::to_string(&state).expect("state JSON");

    for forbidden in ["alice@example.com", "codex@example.com"] {
        assert!(!serialized.contains(forbidden), "leaked {forbidden}");
    }

    let account_ids: Vec<_> = state["usage"]["accounts"]
        .as_array()
        .expect("accounts")
        .iter()
        .map(|account| account["id"].as_str().expect("opaque account id"))
        .collect();
    assert!(account_ids.iter().all(|id| id.starts_with("account-")));
    assert!(state["usage"]["current_by_group"]
        .as_object()
        .expect("current groups")
        .values()
        .all(|id| account_ids.contains(&id.as_str().expect("current handle"))));
    assert!(state["statistics"]["health"]
        .as_array()
        .expect("health")
        .iter()
        .all(|row| account_ids.contains(&row["id"].as_str().expect("health handle"))));
}

#[test]
fn typed_refresh_effect_is_emitted_without_connection_credentials() {
    let mut core = Core::new(DeriveOptions::default());
    let effects = core.reduce(Action::RefreshRequested {
        source: RefreshSource::Manual,
    });
    assert!(matches!(
        effects.as_slice(),
        [Effect::FetchDashboard { request_id }] if request_id == "dashboard-1"
    ));
}

#[test]
fn secret_string_is_cleared_on_drop_by_contract() {
    fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}
    assert_zeroize_on_drop::<SecretString>();
}

#[test]
fn schema_rejects_missing_statistics_platform_and_about_contract_fields() {
    let schema: Value = serde_json::from_str(include_str!("../contract/ui-contract.schema.json"))
        .expect("schema JSON");

    let mut missing_health = state_json();
    missing_health["statistics"]["health"][0]
        .as_object_mut()
        .expect("health row")
        .remove("cooldown_until_ms");
    assert!(schema_errors(&schema, &missing_health)
        .iter()
        .any(|error| error.contains("missing required key cooldown_until_ms")));

    let mut malformed_model = state_json();
    malformed_model["statistics"]["models"][0]
        .as_object_mut()
        .expect("model row")
        .remove("last_used_ms");
    assert!(schema_errors(&schema, &malformed_model)
        .iter()
        .any(|error| error.contains("missing required key last_used_ms")));

    let mut malformed_token_expiry = state_json();
    malformed_token_expiry["usage"]["accounts"][0]["token_expiry"]
        .as_object_mut()
        .expect("token expiry")
        .remove("expires_at_ms");
    assert!(schema_errors(&schema, &malformed_token_expiry)
        .iter()
        .any(|error| error.contains("missing required key expires_at_ms")));

    let mut malformed_screen = state_json();
    malformed_screen["settings"]["screens"] = serde_json::json!([{
        "id": "screen-1",
        "label": "Primary"
    }]);
    assert!(schema_errors(&schema, &malformed_screen)
        .iter()
        .any(|error| error.contains("missing required key selected")));

    let mut missing_about = state_json();
    missing_about["settings"]["maintenance"]
        .as_object_mut()
        .expect("maintenance")
        .remove("islands_version");
    assert!(schema_errors(&schema, &missing_about)
        .iter()
        .any(|error| error.contains("missing required key islands_version")));

    let mut malformed_capability = state_json();
    malformed_capability["settings"]["capabilities"]["tray"]
        .as_object_mut()
        .expect("tray capability")
        .remove("reason");
    assert!(schema_errors(&schema, &malformed_capability)
        .iter()
        .any(|error| error.contains("missing required key reason")));
}

fn schema_errors(schema: &Value, state: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    validate(schema, schema, state, "$", &mut errors);
    errors
}

fn validate(root: &Value, schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let pointer = reference.strip_prefix('#').unwrap_or(reference);
        match root.pointer(pointer) {
            Some(target) => validate(root, target, value, path, errors),
            None => errors.push(format!("{path}: unresolved ref {reference}")),
        }
        return;
    }
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        let valid = branches.iter().any(|branch| {
            let mut branch_errors = Vec::new();
            validate(root, branch, value, path, &mut branch_errors);
            branch_errors.is_empty()
        });
        if !valid {
            errors.push(format!("{path}: no anyOf branch matched"));
        }
        return;
    }
    if let Some(expected) = schema.get("const") {
        if value != expected {
            errors.push(format!("{path}: expected const {expected}, got {value}"));
        }
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.contains(value) {
            errors.push(format!("{path}: {value} is outside enum"));
        }
    }
    if let Some(types) = schema.get("type") {
        let matches = match types {
            Value::String(kind) => type_matches(kind, value),
            Value::Array(kinds) => kinds
                .iter()
                .filter_map(Value::as_str)
                .any(|kind| type_matches(kind, value)),
            _ => true,
        };
        if !matches {
            errors.push(format!("{path}: wrong type for {types}, got {value}"));
            return;
        }
    }
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
        if value.as_f64().is_some_and(|actual| actual < minimum) {
            errors.push(format!("{path}: below minimum {minimum}"));
        }
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
        if value.as_f64().is_some_and(|actual| actual > maximum) {
            errors.push(format!("{path}: above maximum {maximum}"));
        }
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    errors.push(format!("{path}: missing required key {key}"));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, child) in object {
            let child_schema = properties
                .and_then(|items| items.get(key))
                .or_else(|| schema.get("additionalProperties").filter(|v| v.is_object()));
            if let Some(child_schema) = child_schema {
                validate(root, child_schema, child, &format!("{path}.{key}"), errors);
            }
        }
    }
    if let (Some(items), Some(values)) = (schema.get("items"), value.as_array()) {
        for (index, child) in values.iter().enumerate() {
            validate(root, items, child, &format!("{path}[{index}]"), errors);
        }
    }
}

fn type_matches(kind: &str, value: &Value) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}
