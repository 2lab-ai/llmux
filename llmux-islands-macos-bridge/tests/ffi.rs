use std::ptr;
use std::slice;

use llmux_islands_macos_bridge::{
    llmux_islands_bridge_abi_version, llmux_islands_bridge_apply_dashboard,
    llmux_islands_bridge_dispatch, llmux_islands_bridge_free, llmux_islands_bridge_new,
    llmux_islands_bridge_state_json, llmux_islands_owned_bytes_free, LlmuxIslandsBridge,
    LlmuxIslandsOwnedBytes, LlmuxIslandsStatus,
};
use serde_json::{json, Value};

const DASHBOARD: &[u8] = include_bytes!("../../llmux-islands-core/fixtures/dashboard-current.json");

struct Handle(*mut LlmuxIslandsBridge);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: The test owns the live handle and drops it once.
        unsafe { llmux_islands_bridge_free(self.0) };
    }
}

fn empty_bytes() -> LlmuxIslandsOwnedBytes {
    LlmuxIslandsOwnedBytes {
        ptr: ptr::null_mut(),
        len: 0,
    }
}

unsafe fn take_string(bytes: &mut LlmuxIslandsOwnedBytes) -> String {
    let value = if bytes.ptr.is_null() {
        String::new()
    } else {
        // SAFETY: The library returned this live allocation and length.
        String::from_utf8(unsafe { slice::from_raw_parts(bytes.ptr, bytes.len) }.to_vec())
            .expect("ABI output must be UTF-8")
    };
    // SAFETY: Ownership of this allocation is returned exactly once.
    unsafe { llmux_islands_owned_bytes_free(bytes) };
    value
}

fn new_bridge(options: &Value) -> Handle {
    let bytes = serde_json::to_vec(options).expect("options JSON");
    let mut bridge = ptr::null_mut();
    let mut error = empty_bytes();
    // SAFETY: Inputs and outputs remain valid for the call.
    let status =
        unsafe { llmux_islands_bridge_new(bytes.as_ptr(), bytes.len(), &mut bridge, &mut error) };
    assert!(status == LlmuxIslandsStatus::Ok);
    assert!(!bridge.is_null());
    assert!(error.ptr.is_null());
    Handle(bridge)
}

fn dispatch(handle: &Handle, action: Value) -> (Value, Value) {
    let bytes = serde_json::to_vec(&action).expect("action JSON");
    let mut state = empty_bytes();
    let mut effects = empty_bytes();
    let mut error = empty_bytes();
    // SAFETY: The handle is live and every buffer remains valid for the call.
    let status = unsafe {
        llmux_islands_bridge_dispatch(
            handle.0,
            bytes.as_ptr(),
            bytes.len(),
            &mut state,
            &mut effects,
            &mut error,
        )
    };
    assert!(status == LlmuxIslandsStatus::Ok);
    assert!(error.ptr.is_null());
    // SAFETY: Both allocations were returned by the successful call.
    unsafe {
        (
            serde_json::from_str(&take_string(&mut state)).expect("state JSON"),
            serde_json::from_str(&take_string(&mut effects)).expect("effects JSON"),
        )
    }
}

fn apply_dashboard(handle: &Handle, request_id: &str) -> (Value, Value) {
    let mut state = empty_bytes();
    let mut effects = empty_bytes();
    let mut error = empty_bytes();
    // SAFETY: The handle and all exact-length buffers remain live for the call.
    let status = unsafe {
        llmux_islands_bridge_apply_dashboard(
            handle.0,
            request_id.as_ptr(),
            request_id.len(),
            DASHBOARD.as_ptr(),
            DASHBOARD.len(),
            1_700_000_003_000,
            &mut state,
            &mut effects,
            &mut error,
        )
    };
    assert!(status == LlmuxIslandsStatus::Ok);
    assert!(error.ptr.is_null());
    // SAFETY: Both allocations were returned by the successful call.
    unsafe {
        (
            serde_json::from_str(&take_string(&mut state)).expect("state JSON"),
            serde_json::from_str(&take_string(&mut effects)).expect("effects JSON"),
        )
    }
}

fn fetch_request_id(effects: &Value) -> String {
    effects
        .as_array()
        .expect("effect array")
        .iter()
        .find(|effect| effect["type"] == "fetch_dashboard")
        .and_then(|effect| effect["request_id"].as_str())
        .expect("fetch_dashboard effect")
        .to_string()
}

#[test]
fn first_dispatch_is_decodable_and_dashboard_is_request_correlated() {
    assert_eq!(llmux_islands_bridge_abi_version(), 1);
    let handle = new_bridge(&json!({
        "connection": {
            "endpoint_display": "https://user:must-not-leak@example.com/?api_key=hidden",
            "remote": true,
            "authenticated": true,
            "api_key_configured": true
        },
        "platform": {
            "selected_screen_id": "primary",
            "sound_id": "glass",
            "show_fable_weekly": false,
            "presentation": "regular"
        }
    }));

    let (initial, effects) = dispatch(&handle, json!({ "type": "app_started" }));
    let overview = initial["statistics"]["overview"]
        .as_object()
        .expect("strict initial overview");
    for key in [
        "requests",
        "ok",
        "errors",
        "tokens_in",
        "tokens_out",
        "rpm_5m",
        "in_flight",
        "cost_usd",
    ] {
        assert!(overview.contains_key(key), "missing initial overview {key}");
    }
    let initial_text = initial.to_string();
    assert!(!initial_text.contains("must-not-leak"));
    assert!(!initial_text.contains("api_key=hidden"));
    assert_eq!(initial["window"]["selected_screen_id"], "primary");
    assert_eq!(initial["settings"]["sound_id"], "glass");
    assert_eq!(initial["settings"]["show_fable_weekly"], false);
    let request_id = fetch_request_id(&effects);

    let (stale, stale_effects) = apply_dashboard(&handle, "dashboard-stale");
    assert_eq!(stale["lifecycle"], "starting");
    assert_eq!(stale_effects, json!([]));

    let (hydrated, hydrated_effects) = apply_dashboard(&handle, &request_id);
    assert_eq!(hydrated["lifecycle"], "ready");
    assert_eq!(hydrated["window"]["selected_screen_id"], "primary");
    assert_eq!(hydrated["settings"]["sound_id"], "glass");
    assert_eq!(hydrated["settings"]["show_fable_weekly"], false);
    assert_eq!(hydrated["statistics"]["overview"]["requests"], 15);
    assert_eq!(
        hydrated["statistics"]["activity_receipts"]
            .as_array()
            .expect("activity receipts")
            .len(),
        3
    );
    assert!(hydrated_effects
        .as_array()
        .expect("effects")
        .iter()
        .any(|effect| effect["type"] == "update_tray"));
    assert!(hydrated["usage"]["accounts"][0]["gauges"]
        .as_array()
        .expect("semantic gauges")
        .iter()
        .any(|gauge| gauge["kind"] == "fable_weekly"));
    let hydrated_text = hydrated.to_string();
    assert!(!hydrated_text.contains("alice@example.com"));
    assert!(!hydrated_text.contains("Bearer-secret"));
    assert!(!hydrated_text.contains("api_key=must-not-leak"));
}

#[test]
fn opaque_account_mutation_returns_raw_executor_effect_and_receipt_survives_refresh() {
    let handle = new_bridge(&json!({}));
    let (_, start_effects) = dispatch(&handle, json!({ "type": "app_started" }));
    let (hydrated, _) = apply_dashboard(&handle, &fetch_request_id(&start_effects));
    let account_handle = hydrated["usage"]["accounts"][0]["id"]
        .as_str()
        .expect("opaque account handle")
        .to_string();
    assert!(account_handle.starts_with("account-"));

    let (busy, effects) = dispatch(
        &handle,
        json!({
            "type": "operation_started",
            "id": "pause-1",
            "request": {
                "kind": "pause_account",
                "account_id": account_handle,
                "paused": true
            },
            "target_display": "opaque selection",
            "started_at_ms": 10
        }),
    );
    assert_eq!(busy["operation"]["id"], "pause-1");
    assert_eq!(effects[0]["type"], "run_operation");
    assert_eq!(effects[0]["request"]["kind"], "pause_account");
    assert_eq!(effects[0]["request"]["account_id"], "alice@example.com");

    let (finished, effects) = dispatch(
        &handle,
        json!({
            "type": "operation_finished",
            "id": "pause-1",
            "outcome": "succeeded",
            "message": "account paused",
            "finished_at_ms": 20
        }),
    );
    assert_eq!(finished["verification_receipts"][0]["id"], "pause-1");
    assert!(!finished.to_string().contains("alice@example.com"));
    let refresh_id = fetch_request_id(&effects);
    let (refreshed, _) = apply_dashboard(&handle, &refresh_id);
    assert_eq!(refreshed["verification_receipts"][0]["id"], "pause-1");
}

#[test]
fn successful_local_setting_effects_update_canonical_state_and_survive_refresh() {
    let handle = new_bridge(&json!({}));
    let (_, start_effects) = dispatch(&handle, json!({ "type": "app_started" }));
    let (mut state, _) = apply_dashboard(&handle, &fetch_request_id(&start_effects));

    let cases = [
        (
            "screen-1",
            json!({ "kind": "persist_screen", "id": "display-42" }),
            "succeeded",
        ),
        (
            "sound-1",
            json!({ "kind": "persist_sound", "id": "glass" }),
            "no_change",
        ),
        (
            "fable-1",
            json!({ "kind": "persist_show_fable", "enabled": false }),
            "succeeded",
        ),
    ];

    for (index, (id, request, outcome)) in cases.into_iter().enumerate() {
        let (_, effects) = dispatch(
            &handle,
            json!({
                "type": "operation_started",
                "id": id,
                "request": request,
                "target_display": id,
                "started_at_ms": 100 + index
            }),
        );
        assert_eq!(effects[0]["type"], "persist_settings");
        let (finished, effects) = dispatch(
            &handle,
            json!({
                "type": "operation_finished",
                "id": id,
                "outcome": outcome,
                "message": "local setting persisted",
                "finished_at_ms": 200 + index
            }),
        );
        state = finished;
        if outcome == "succeeded" {
            state = apply_dashboard(&handle, &fetch_request_id(&effects)).0;
        } else {
            assert_eq!(effects, json!([]));
        }
    }

    assert_eq!(state["window"]["selected_screen_id"], "display-42");
    assert_eq!(state["settings"]["sound_id"], "glass");
    assert_eq!(state["settings"]["show_fable_weekly"], false);
    assert!(state["usage"]["accounts"][0]["gauges"]
        .as_array()
        .expect("semantic gauges")
        .iter()
        .any(|gauge| gauge["kind"] == "fable_weekly"));
}

#[test]
fn failed_and_stale_local_setting_completions_do_not_change_canonical_state() {
    let handle = new_bridge(&json!({}));
    let (initial, effects) = dispatch(
        &handle,
        json!({
            "type": "operation_started",
            "id": "screen-failed",
            "request": { "kind": "persist_screen", "id": "must-not-apply" },
            "target_display": "must-not-apply",
            "started_at_ms": 10
        }),
    );
    assert_eq!(initial["window"]["selected_screen_id"], "auto");
    assert_eq!(effects[0]["type"], "persist_settings");

    let (stale, stale_effects) = dispatch(
        &handle,
        json!({
            "type": "operation_finished",
            "id": "different-operation",
            "outcome": "succeeded",
            "message": "stale",
            "finished_at_ms": 11
        }),
    );
    assert_eq!(stale["window"]["selected_screen_id"], "auto");
    assert_eq!(stale["operation"]["id"], "screen-failed");
    assert_eq!(stale_effects, json!([]));

    let (failed, failed_effects) = dispatch(
        &handle,
        json!({
            "type": "operation_finished",
            "id": "screen-failed",
            "outcome": "failed",
            "message": "not persisted",
            "finished_at_ms": 12
        }),
    );
    assert_eq!(failed["window"]["selected_screen_id"], "auto");
    assert_eq!(failed_effects, json!([]));
}

#[test]
fn api_key_is_a_presence_marker_and_never_appears_in_state_effect_or_error() {
    let handle = new_bridge(&json!({}));
    let secret = "sk-live-never-cross-bridge";
    let (state, effects) = dispatch(
        &handle,
        json!({
            "type": "operation_started",
            "id": "add-1",
            "request": {
                "kind": "add_account",
                "name": "work",
                "has_api_key": true
            },
            "target_display": "work",
            "started_at_ms": 10
        }),
    );
    assert_eq!(effects[0]["request"]["api_key_required"], true);
    assert!(!state.to_string().contains(secret));
    assert!(!effects.to_string().contains(secret));

    let invalid = json!({
        "type": "operation_started",
        "id": "bad-add",
        "request": {
            "kind": "add_account",
            "name": "work",
            "api_key": secret
        },
        "target_display": "work",
        "started_at_ms": 11
    });
    let bytes = serde_json::to_vec(&invalid).expect("invalid action JSON");
    let mut invalid_state = empty_bytes();
    let mut invalid_effects = empty_bytes();
    let mut error = empty_bytes();
    // SAFETY: Inputs and outputs remain live for the call.
    let status = unsafe {
        llmux_islands_bridge_dispatch(
            handle.0,
            bytes.as_ptr(),
            bytes.len(),
            &mut invalid_state,
            &mut invalid_effects,
            &mut error,
        )
    };
    assert!(status == LlmuxIslandsStatus::InvalidAction);
    // SAFETY: error was returned by the failed call.
    let error = unsafe { take_string(&mut error) };
    assert!(!error.contains(secret));
    assert!(error.contains("invalid_action"));
}

#[test]
fn login_state_is_executor_only_and_transient_failure_preserves_device_data() {
    let handle = new_bridge(&json!({}));
    let (_, effects) = dispatch(
        &handle,
        json!({
            "type": "login_started",
            "operation_id": "login-1",
            "provider": "grok",
            "started_at_ms": 100
        }),
    );
    assert_eq!(effects[0]["type"], "start_login");

    let oauth_state = "daemon-oauth-state-must-not-render";
    let (pending, effects) = dispatch(
        &handle,
        json!({
            "type": "login_status_received",
            "operation_id": "login-1",
            "status": {
                "phase": "pending",
                "state": oauth_state,
                "verification_uri": "https://x.ai/device?secret=ephemeral",
                "user_code": "ABCD-EFGH",
                "message": "waiting"
            },
            "at_ms": 110
        }),
    );
    assert_eq!(pending["usage"]["login"]["state"], "active");
    assert_eq!(
        pending["usage"]["login"]["verification_uri"],
        "https://x.ai/device"
    );
    assert!(!pending.to_string().contains(oauth_state));
    assert_eq!(effects[0]["type"], "poll_login");
    assert_eq!(effects[0]["state"], oauth_state);

    let (retrying, _) = dispatch(
        &handle,
        json!({
            "type": "login_status_received",
            "operation_id": "login-1",
            "status": {
                "phase": "pending",
                "state": oauth_state,
                "verification_uri": null,
                "user_code": null,
                "message": "login status unavailable; retrying"
            },
            "at_ms": 120
        }),
    );
    assert_eq!(
        retrying["usage"]["login"]["verification_uri"],
        "https://x.ai/device"
    );
    assert_eq!(retrying["usage"]["login"]["user_code"], "ABCD-EFGH");

    let (cancelling, effects) = dispatch(
        &handle,
        json!({
            "type": "login_cancel_requested",
            "operation_id": "login-1"
        }),
    );
    assert_eq!(cancelling["usage"]["login"]["phase"], "cancelling");
    assert_eq!(effects[0]["type"], "stop_login_poll");
    assert_eq!(effects[1]["type"], "cancel_login");
    assert_eq!(effects[1]["state"], oauth_state);
}

#[test]
fn null_invalid_utf8_oversized_inputs_and_double_free_are_bounded() {
    let mut bridge = ptr::null_mut();
    let mut error = empty_bytes();
    // SAFETY: Output pointers are valid; null input is intentionally tested.
    let status = unsafe { llmux_islands_bridge_new(ptr::null(), 1, &mut bridge, &mut error) };
    assert!(status == LlmuxIslandsStatus::InvalidArgument);
    assert!(bridge.is_null());
    // SAFETY: error was returned by the failed call.
    assert!(unsafe { take_string(&mut error) }.contains("invalid_argument"));

    let handle = new_bridge(&json!({}));
    let invalid_utf8 = [0xff_u8];
    let mut state = empty_bytes();
    let mut effects = empty_bytes();
    let mut invalid_error = empty_bytes();
    // SAFETY: Every pointer is valid for its advertised length.
    let status = unsafe {
        llmux_islands_bridge_dispatch(
            handle.0,
            invalid_utf8.as_ptr(),
            invalid_utf8.len(),
            &mut state,
            &mut effects,
            &mut invalid_error,
        )
    };
    assert!(status == LlmuxIslandsStatus::InvalidJson);
    // SAFETY: invalid_error was returned by the failed call.
    assert!(!unsafe { take_string(&mut invalid_error) }.contains("ff"));

    let one_byte = [b'{'];
    // SAFETY: Oversize is rejected before the library dereferences the input.
    let status = unsafe {
        llmux_islands_bridge_dispatch(
            handle.0,
            one_byte.as_ptr(),
            256 * 1024 + 1,
            &mut state,
            &mut effects,
            &mut invalid_error,
        )
    };
    assert!(status == LlmuxIslandsStatus::InvalidArgument);
    // SAFETY: invalid_error was returned by the failed call.
    unsafe { take_string(&mut invalid_error) };

    let mut current_state = empty_bytes();
    let mut state_error = empty_bytes();
    // SAFETY: The handle and outputs are live.
    let status =
        unsafe { llmux_islands_bridge_state_json(handle.0, &mut current_state, &mut state_error) };
    assert!(status == LlmuxIslandsStatus::Ok);
    // SAFETY: current_state owns a live library allocation.
    unsafe {
        llmux_islands_owned_bytes_free(&mut current_state);
        llmux_islands_owned_bytes_free(&mut current_state);
    }
    assert!(current_state.ptr.is_null());
    assert_eq!(current_state.len, 0);
}
