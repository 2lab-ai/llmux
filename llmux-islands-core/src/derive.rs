use std::collections::{BTreeMap, BTreeSet};

use llmux::dashboard::{AccountDoc, DashboardDoc, ScopedWindowDoc, WindowDoc};
use serde_json::{json, Value};

use crate::contract::{
    AccountTile, DeriveOptions, Gauge, GaugeKind, Lifecycle, Provider, TokenExpiry, UiState,
    WarningLevel,
};
use crate::privacy::{display_account, sanitize_endpoint, sanitize_json_strings, sanitize_text};
use crate::receipts::from_activity_with_account_privacy;

/// Pure DashboardDoc -> semantic UI projection. Platform-only values enter
/// through DeriveOptions; credentials never do.
pub fn derive_ui_state(document: &DashboardDoc, options: &DeriveOptions, now_ms: u64) -> UiState {
    let account_handles = document_account_handles(document);
    derive_ui_state_with_account_handles(document, options, now_ms, &account_handles)
}

pub(crate) fn derive_ui_state_with_account_handles(
    document: &DashboardDoc,
    options: &DeriveOptions,
    now_ms: u64,
    account_handles: &BTreeMap<String, String>,
) -> UiState {
    let mut state = UiState::initial(options);
    state.revision = 1;
    state.lifecycle = Lifecycle::Ready;
    state.connection.endpoint_display = sanitize_endpoint(&options.endpoint_display);
    state.connection.daemon_version = Some(sanitize_text(&document.version));
    state.connection.last_success_ms = Some(now_ms);
    state.connection.retry_at_ms = None;
    state.connection.error = None;

    let raw_current_by_group = effective_current_by_group(document);
    let current_accounts: BTreeSet<&str> =
        raw_current_by_group.values().map(String::as_str).collect();
    let mut provider_in_flight = BTreeMap::new();
    state.usage.accounts = document
        .accounts
        .iter()
        .map(|account| {
            let provider = Provider::from_account_kind(&account.kind);
            let count = provider_in_flight
                .entry(provider.key().to_string())
                .or_insert(0_u32);
            *count = count.saturating_add(account.in_flight);
            account_tile(
                account,
                exposed_account_id(account_handles, &account.name),
                provider,
                current_accounts.contains(account.name.as_str()),
                document,
                now_ms,
            )
        })
        .collect();
    state.usage.current_by_group = raw_current_by_group
        .into_iter()
        .filter_map(|(group, account)| {
            account_handles
                .get(&account)
                .cloned()
                .map(|handle| (group, handle))
        })
        .collect();
    state
        .usage
        .provider_in_flight
        .clone_from(&provider_in_flight);
    state.window.provider_in_flight = provider_in_flight;

    state.statistics.overview = json!({
        "requests": document.totals.requests,
        "ok": document.totals.ok,
        "errors": document.totals.errors,
        "tokens_in": document.totals.tokens_in,
        "tokens_out": document.totals.tokens_out,
        "rpm_5m": document.totals.rpm_5m.max(0.0),
        "in_flight": document.totals.in_flight,
        "cost_usd": document.totals.cost_usd.max(0.0)
    });
    state.statistics.models = model_rows(document);
    state.statistics.clients = client_rows(document);
    state.statistics.health = health_rows(document, account_handles);
    state.statistics.heatmaps = heatmap_rows(document);
    state.statistics.activity_receipts = from_activity_with_account_privacy(
        &document.activity,
        now_ms,
        document.email_anonymous,
        account_handles,
    );
    state.statistics.data_quality = value_or_empty(&document.data_quality);

    state.settings.email_anonymous = document.email_anonymous;
    state.settings.show_fable_weekly = document.show_fable_weekly;
    state.settings.api_key_configured = options.api_key_configured;
    state.settings.events = document
        .events
        .iter()
        .map(|event| {
            let mut value = value_or_empty(event);
            sanitize_json_strings(&mut value);
            value
        })
        .collect();
    state.settings.capabilities = json!({
        "presentation": options.presentation,
        "remote": options.remote,
        "layer_shell": {
            "available": options.presentation == crate::contract::Presentation::LayerShell,
            "reason": match options.presentation {
                crate::contract::Presentation::LayerShell => "Layer-shell presentation is active",
                crate::contract::Presentation::PositionedX11 => "X11 uses a positioned fallback",
                crate::contract::Presentation::Regular => "A regular-window fallback is active",
            }
        },
        "tray": {
            "available": false,
            "reason": "platform capability not reported"
        },
        "notifications": {
            "available": false,
            "reason": "platform capability not reported"
        }
    });

    state
}

fn document_account_handles(document: &DashboardDoc) -> BTreeMap<String, String> {
    document
        .accounts
        .iter()
        .enumerate()
        .map(|(index, account)| {
            let exposed = if document.email_anonymous {
                format!("account-{}", index.saturating_add(1))
            } else {
                account.name.clone()
            };
            (account.name.clone(), exposed)
        })
        .collect()
}

fn exposed_account_id(account_handles: &BTreeMap<String, String>, raw_id: &str) -> String {
    account_handles
        .get(raw_id)
        .cloned()
        .unwrap_or_else(|| "account-unavailable".to_string())
}

fn effective_current_by_group(document: &DashboardDoc) -> BTreeMap<String, String> {
    if !document.current_by_group.is_empty() {
        return document.current_by_group.clone();
    }
    document
        .current
        .as_ref()
        .map(|account| BTreeMap::from([("claude".to_string(), account.clone())]))
        .unwrap_or_default()
}

fn account_tile(
    account: &AccountDoc,
    exposed_id: String,
    provider: Provider,
    current: bool,
    document: &DashboardDoc,
    now_ms: u64,
) -> AccountTile {
    let gauges = vec![
        window_gauge(
            GaugeKind::FiveHour,
            account.five_hour.as_ref(),
            document.select_params.five_hour_max,
        ),
        window_gauge(
            GaugeKind::SevenDay,
            account.seven_day.as_ref(),
            document.select_params.seven_day_max,
        ),
        // Fable visibility is a platform-local presentation preference. Keep
        // the semantic gauge hydrated so turning it back on never requires a
        // daemon document that happened to expose it while visibility was on.
        scoped_gauge(account.fable_weekly.as_ref()),
    ];
    let fable_critical = account
        .fable_weekly
        .as_ref()
        .is_some_and(|gauge| gauge.constraining && gauge.severity.eq_ignore_ascii_case("critical"));
    let warning_level = if fable_critical {
        WarningLevel::Critical
    } else if !account.healthy || account.blocked.is_some() || account.paused {
        WarningLevel::Warning
    } else {
        WarningLevel::Normal
    };
    AccountTile {
        id: exposed_id,
        display_name: display_account(&account.name, document.email_anonymous),
        provider,
        current,
        paused: account.paused,
        healthy: account.healthy,
        status: sanitize_text(&account.status),
        blocked_reason: account.blocked.as_deref().map(sanitize_text),
        in_flight: account.in_flight,
        token_expiry: account
            .token_expires_at_ms
            .map(|expires_at_ms| token_expiry(expires_at_ms, now_ms)),
        gauges,
        warning_level,
        busy_action: None,
    }
}

fn window_gauge(kind: GaugeKind, window: Option<&WindowDoc>, limit: f64) -> Gauge {
    match window {
        Some(window) => {
            let used = clamp_fraction(window.utilization);
            Gauge {
                kind,
                available: true,
                used_fraction: used,
                remaining_fraction: 1.0 - used,
                resets_at: Some(window.resets_at.saturating_mul(1_000)),
                reset_text: Some(format_duration(window.resets_in_secs)),
                constraining: used >= clamp_fraction(limit),
            }
        }
        None => Gauge {
            kind,
            available: false,
            used_fraction: 0.0,
            remaining_fraction: 0.0,
            resets_at: None,
            reset_text: None,
            constraining: false,
        },
    }
}

fn scoped_gauge(window: Option<&ScopedWindowDoc>) -> Gauge {
    match window {
        Some(window) => {
            let used = clamp_fraction(window.utilization);
            Gauge {
                kind: GaugeKind::FableWeekly,
                available: true,
                used_fraction: used,
                remaining_fraction: 1.0 - used,
                resets_at: Some(window.resets_at.saturating_mul(1_000)),
                reset_text: Some(format_duration(window.resets_in_secs)),
                constraining: window.constraining,
            }
        }
        None => Gauge {
            kind: GaugeKind::FableWeekly,
            available: false,
            used_fraction: 0.0,
            remaining_fraction: 0.0,
            resets_at: None,
            reset_text: None,
            constraining: false,
        },
    }
}

fn token_expiry(expires_at_ms: u64, now_ms: u64) -> TokenExpiry {
    let remaining_secs = expires_at_ms.saturating_sub(now_ms) / 1_000;
    let state = if expires_at_ms <= now_ms {
        "expired"
    } else if remaining_secs <= 3_600 {
        "expiring"
    } else {
        "valid"
    };
    TokenExpiry {
        state: state.to_string(),
        expires_at_ms,
        countdown_text: format_duration(remaining_secs),
    }
}

fn model_rows(document: &DashboardDoc) -> Vec<Value> {
    document
        .model_usage
        .iter()
        .map(|row| {
            let accounts: Vec<Value> = row
                .accounts
                .iter()
                .map(|account| {
                    json!({
                        "display_name": display_account(&account.name, document.email_anonymous),
                        "requests": account.requests,
                        "ok": account.ok,
                        "errors": account.errors,
                        "tokens_in": account.tokens_in,
                        "tokens_out": account.tokens_out
                    })
                })
                .collect();
            json!({
                "group": sanitize_text(&row.group),
                "model": sanitize_text(&row.model),
                "requests": row.requests,
                "ok": row.ok,
                "errors": row.errors,
                "tokens_in": row.tokens_in,
                "tokens_out": row.tokens_out,
                "cache_read": row.cache_read,
                "cache_creation": row.cache_creation,
                "last_used_ms": row.last_used_ms,
                "in_flight": row.in_flight,
                "accounts": accounts,
                "efforts": value_or_empty(&row.efforts),
                "endpoints": value_or_empty(&row.endpoints),
                "cost_usd": row.cost_usd.max(0.0)
            })
        })
        .collect()
}

fn client_rows(document: &DashboardDoc) -> Vec<Value> {
    document
        .client_usage
        .iter()
        .map(|row| {
            json!({
                "client": sanitize_text(&row.client),
                "requests": row.requests,
                "ok": row.ok,
                "errors": row.errors,
                "tokens_in": row.tokens_in,
                "tokens_out": row.tokens_out,
                "cost_usd": row.cost_usd.max(0.0),
                "last_seen_ms": row.last_seen_ms
            })
        })
        .collect()
}

fn health_rows(document: &DashboardDoc, account_handles: &BTreeMap<String, String>) -> Vec<Value> {
    document
        .accounts
        .iter()
        .map(|account| {
            json!({
                "id": exposed_account_id(account_handles, &account.name),
                "display_name": display_account(&account.name, document.email_anonymous),
                "kind": sanitize_text(&account.kind),
                "healthy": account.healthy,
                "paused": account.paused,
                "status": sanitize_text(&account.status),
                "blocked_reason": account.blocked.as_deref().map(sanitize_text),
                "cooldown_until_ms": account.cooldown_until.map(|seconds| seconds.saturating_mul(1_000)),
                "cooldown_source": account.cooldown_source.as_deref().map(sanitize_text),
                "in_flight": account.in_flight,
                "token_expires_at_ms": account.token_expires_at_ms,
                "last_refresh_ms": account.last_refresh_ms
            })
        })
        .collect()
}

fn heatmap_rows(document: &DashboardDoc) -> Vec<Value> {
    document
        .windowed
        .iter()
        .map(|window| {
            let cells: Vec<Value> = window
                .cells
                .iter()
                .map(|cell| {
                    json!({
                        "group": sanitize_text(&cell.group),
                        "model": sanitize_text(&cell.model),
                        "account_display": display_account(&cell.account, document.email_anonymous),
                        "requests": cell.requests,
                        "ok": cell.ok,
                        "errors": cell.errors,
                        "tokens_in": cell.tokens_in,
                        "tokens_out": cell.tokens_out,
                        "cache_read": cell.cache_read,
                        "cache_creation": cell.cache_creation,
                        "tokens": cell.tokens
                    })
                })
                .collect();
            json!({
                "window": sanitize_text(&window.window),
                "window_secs": window.window_secs,
                "cells": cells
            })
        })
        .collect()
}

fn value_or_empty<T: serde::Serialize>(value: &T) -> Value {
    match serde_json::to_value(value) {
        Ok(value) => value,
        Err(_) => json!({}),
    }
}

fn clamp_fraction(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}
