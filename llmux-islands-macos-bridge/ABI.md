# llmux Islands macOS bridge ABI v1

The canonical declarations are in
`include/llmux_islands_macos_bridge.h`. All JSON inputs are exact-length UTF-8;
all returned bytes are owned by Rust until passed to
`llmux_islands_owned_bytes_free`.

## Initialization

```json
{
  "connection": {
    "endpoint_display": "http://127.0.0.1:3456",
    "remote": false,
    "authenticated": true,
    "api_key_configured": false
  },
  "platform": {
    "selected_screen_id": "auto",
    "sound_id": "default",
    "show_fable_weekly": true,
    "presentation": "regular"
  }
}
```

`presentation` is `regular`, `positioned_x11`, or `layer_shell`. The bridge
uses `sound_id` and `show_fable_weekly` as optional platform-local state seeds;
successful matching persistence receipts update those values without letting a
later daemon refresh overwrite them. The bridge
accepts no connection credential. Endpoint user-info/query/fragment data and
token-shaped screen labels are removed before the initial `UiState` exists.

## Dashboard cycle

1. Dispatch `{"type":"app_started"}` once, or
   `{"type":"refresh_requested","source":"manual"}` for later refreshes.
2. Consume the `fetch_dashboard` effect and retain its `request_id`.
3. Fetch `/llmux/dashboard` and pass the exact id and raw response bytes to
   `llmux_islands_bridge_apply_dashboard`.

The shared reducer ignores stale ids. Never invent an id or apply a response to
the newest id by arrival order.

## Actions

Every action is tagged by top-level `type`:

| `type` | fields |
|---|---|
| `app_started`, `tray_activated`, `close_requested` | none |
| `open_requested` | `reason`: `click`, `hover`, `notification`, `usage_alert`, or `boot` |
| `navigation_selected` | `navigation`: `usage`, `statistics`, or `menu` |
| `window_metrics_changed` | `width`, `content_height` |
| `refresh_requested` | `source`: `startup`, `manual`, `poll`, `retry`, or `mutation` |
| `dashboard_failed` | `request_id`, `error`, `failed_at_ms` |
| `settings_changed` | `id`, `email_anonymous`, `started_at_ms` |
| `login_started` | `operation_id`, `provider`, `started_at_ms` |
| `login_status_received` | `operation_id`, nested `status`, `at_ms` |
| `login_cancel_requested` | `operation_id` |
| `operation_started` | `id`, nested `request`, optional `target_display`, `started_at_ms` |
| `operation_finished` | `id`, `outcome`, `message`, `finished_at_ms` |

Login status is tagged by `phase`: `pending`, `succeeded`, `failed`,
`cancelled`, `cancellation_acknowledged`, or `cancellation_failed`. Pending has
`state`, nullable `verification_uri`, nullable `user_code`, and nullable
`message`. Succeeded has nullable `target_display` plus `message`; other
terminal phases have `message`.

Operation requests are tagged by nested `kind`:

```json
{"kind":"add_account","name":"work","has_api_key":true}
{"kind":"pause_account","account_id":"account-1","paused":true}
{"kind":"remove_account","account_id":"account-1","confirmed":true}
{"kind":"update_settings","email_anonymous":true}
{"kind":"upsert_event","event":{"id":"e1","from":"...","to":"...","content":"..."}}
{"kind":"remove_event","event_id":"e1"}
{"kind":"persist_screen","id":"primary"}
{"kind":"persist_sound","id":"default"}
{"kind":"persist_show_fable","enabled":true}
{"kind":"persist_connection","endpoint":"https://daemon.example","api_key_configured":true}
{"kind":"set_autostart","enabled":true}
{"kind":"run_maintenance","command":{"kind":"update"}}
```

`operation_finished.outcome` is `succeeded`, `failed`, `cancelled`, or
`no_change`.

The API key remains in Swift, keyed by operation id. Only its presence crosses
the ABI. The executor uses its retained value when an add-account effect says
`api_key_required: true`.

## Effects

Effects are an executor-only array. Each element has top-level `type`:

- `ensure_local_daemon`
- `fetch_dashboard` (`request_id`)
- `schedule_dashboard_retry` (`retry_at_ms`)
- `cancel_dashboard_retry`
- `start_login` (`operation_id`, `provider`)
- `poll_login` / `cancel_login` (`operation_id`, executor-only `state`)
- `stop_login_poll` (`operation_id`)
- `run_operation` (`operation_id`, nested `request` tagged by `kind`)
- `update_settings`, `upsert_event`, `remove_event`
- `persist_settings` (nested `change` tagged by `kind`)
- `set_autostart`, `run_maintenance`, `update_tray`

For pause/remove, the input account id is the opaque id from `UiState`; the
`run_operation` effect contains the daemon's raw account id. This effect is the
only authorized mapping boundary. Effects may contain raw account ids or OAuth
state and therefore must be consumed immediately, never rendered, logged, or
persisted. Canonical `UiState` and errors never contain those executor values.

## Build

```sh
MACOSX_DEPLOYMENT_TARGET=14.0 \
  cargo build --manifest-path llmux-islands-macos-bridge/Cargo.toml \
  --release --locked --offline
```

The host artifact is
`llmux-islands-macos-bridge/target/release/libllmux_islands_macos_bridge.a`.
Build `--target aarch64-apple-darwin` and `--target x86_64-apple-darwin`
separately and combine with `lipo` when both Rust targets are installed.
The final application link must include `Security.framework` and
`CoreFoundation.framework`.
