//! `DashboardView` — the one struct the draw code renders from, built from a
//! [`DashboardDoc`] regardless of where that document came from: the local
//! TUI builds it in-process from live `AppState` and the attach-mode client
//! parses it from `GET /llmux/dashboard` JSON. One contract, one
//! renderer — the rendering is never forked.

use std::collections::HashMap;
use std::str::FromStr as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::dashboard::{CompletedDoc, DashboardDoc, WindowDoc};
use crate::logging::LogLine;
use crate::scheduler::select::SelectParams;
use crate::scheduler::window::{LimitSeverity, QuotaWindow, ScopedQuotaWindow, WindowSource};
use crate::scheduler::{AccountId, AccountSnapshot, CooldownSource, PoolSnapshot};

use super::activity::{Completed, CompletedBody, InFlight, Totals};
use super::{LastSwitch, PollHealth, TokenCounts};

/// Everything one frame renders. Owned (no borrow into app state) so a
/// remote document and live local state produce the identical input.
pub(crate) struct DashboardView {
    pub version: String,
    pub pid: u32,
    pub uptime: Duration,
    pub port: u16,
    pub upstream: Option<String>,
    pub config_path: Option<String>,
    pub select_params: SelectParams,
    pub refresh_ahead: Duration,
    pub evaluate_tick: Duration,
    pub snapshot: PoolSnapshot,
    pub last_switch: Option<LastSwitch>,
    pub poll_health: HashMap<String, PollHealth>,
    /// Per-account activity totals (table req/tok columns, detail pane).
    pub session_totals: HashMap<String, Totals>,
    pub global_totals: Totals,
    pub rpm_5m: f64,
    /// Oldest→newest (rendered reversed: newest start on top).
    pub in_flight: Vec<InFlight>,
    /// Newest first.
    pub completed: Vec<Completed>,
    /// Derived session titles (TUI UI-3 U2): client `user_id` → first
    /// user-input excerpt, carried from the document.
    pub session_labels: std::collections::BTreeMap<String, String>,
    /// Oldest→newest tail.
    pub logs: Vec<LogLine>,
    /// Per-(group, model) usage rows (req1-20), already sorted by total tokens.
    /// One representation — the serializable doc row — used by both the
    /// document and the renderer, so local and attach render identically.
    pub model_usage: Vec<crate::dashboard::ModelUsageDoc>,
    /// Per-client request attribution rows (issue #32), already sorted by
    /// requests desc. One representation used by both document and renderer.
    pub client_usage: Vec<crate::dashboard::ClientUsageDoc>,
    /// Windowed (24h/72h) per-account/per-model heatmap slices (issue #23),
    /// carried straight from the document so local + attach render the same.
    /// Best-effort: a lossy sample of the activity stream, not an exact ledger.
    pub windowed: Vec<crate::dashboard::WindowedStatsDoc>,
    /// Live codex settings (req8.1): shown + toggled from the dashboard.
    pub codex: crate::dashboard::CodexSettingsDoc,
    /// Live grok settings (UI-3 U12): shown + cycled from the group bar.
    pub grok: crate::dashboard::GrokSettingsDoc,
    /// Tokens-per-day chart rows (UI-3 U14), straight from the document.
    pub daily_usage: Vec<crate::dashboard::DailyUsageDoc>,
    /// Observed-performance rows (perf telemetry v1), oldest day first.
    pub daily_perf: Vec<crate::dashboard::DailyPerfDoc>,
    /// Config-editor facts (config-editor v1).
    pub config_facts: crate::dashboard::ConfigFactsDoc,
    /// Usage-tab calendar rows (usage-stats), straight from the document —
    /// labels and cost are server-rendered, the client only filters by
    /// granularity and draws.
    pub usage_stats: Vec<crate::dashboard::UsageStatDoc>,
    /// Live `email_anonymous` display setting (SSOT E4): when on, every
    /// draw-time surface that shows an account email renders it through
    /// [`crate::demo::alias_always`] / [`crate::demo::mask_email_text`]
    /// instead of raw. The view's `snapshot` keeps REAL ids so interactive
    /// paths (switch/remove target names) still address the pool correctly —
    /// masking happens strictly at the render sites in `ui.rs`.
    pub email_anonymous: bool,
    /// Whether the TUI plays cosmetic animations (config `tui_effects`, carried
    /// on the document, default ON): the effort-token rainbow marquee (`max`)
    /// and the headline-model name gradient (`fable-5*`/`gpt-5.6-sol*`). When
    /// off those tokens keep a distinct STATIC color+bold; working spinners
    /// animate regardless (they predate this knob). Render-only gate.
    pub tui_effects: bool,
    /// Gradient drift speed + base colors (config `tui_gradient`, UI-8),
    /// pre-resolved at view-build time into render-ready values: unparseable
    /// hex falls back to the built-in bases, non-finite/non-positive speed to
    /// 1.0 — so `ui.rs` never re-validates per frame.
    pub gradient: crate::tui::ui::GradientCfg,
    /// Whether the accounts table renders the model-scoped "Fable" weekly gauge
    /// (fable-usage U9a — config `show_fable_weekly`, default ON). Carried on
    /// the view so the one shared renderer honors it in both TUI backends;
    /// when off the table renders exactly as before W3. The scoped data itself
    /// always reaches the view (see [`AccountSnapshot::scoped_limits`] rebuilt
    /// below) — this flag only gates the render.
    pub show_fable_weekly: bool,
    /// Accounts-table domain abbreviations (config `domain_abbrev`, carried on
    /// the document): render `ai3@insightquest.io` as `ai3@iq.io`. Render-only
    /// — the snapshot keeps real ids, same layering as `email_anonymous`.
    pub domain_abbrev: std::collections::BTreeMap<String, String>,
    /// Boot default for the quota-gauge fill direction (config
    /// `quota_display`, carried on the document). The TUI `u` key holds a
    /// session-local override in `Chrome`; the effective mode is resolved per
    /// frame in `ui::draw`.
    pub quota_display: crate::config::QuotaDisplay,
    /// Data-quality label wording (issue #62 S2), carried verbatim from the
    /// document — the server owns the wording, `ui.rs` renders the cost /
    /// model-usage-scope qualifiers from these strings. A doc from an older
    /// daemon fills the byte-identical canonical defaults via serde.
    pub data_quality: crate::dashboard::DataQualityDoc,
    /// Top-of-dashboard event banners (config `events`). Carried through the
    /// `/llmux/dashboard` document from the daemon's live holder
    /// ([`crate::proxy::server::AppState::event_banners`]), so they populate in
    /// BOTH backends (local builds the doc in-process; attach receives it) and a
    /// `POST /llmux/events` reflects on the next frame/poll. `ui.rs` renders the
    /// active banner (`from <= now < to`) with the earliest `to` as one line,
    /// and nothing when none is active.
    pub events: Vec<crate::config::EventBanner>,
    /// Rolling 5-minute status-class counts for the header health verdict
    /// (glance-triage), carried on the document. `None` from an older daemon
    /// = no telemetry: the verdict skips storm detection and renders the err
    /// surface as unavailable — never a fabricated healthy zero.
    pub health: Option<super::activity::HealthCounts>,
}

fn ms_time(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

fn secs_time(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

/// Map a serialized credential kind back to the static str the scheduler's
/// pure functions compare against. Unknown kinds (newer server) degrade to a
/// label that matches no special case.
fn kind_static(kind: &str) -> &'static str {
    match kind {
        "oauth" => "oauth",
        "apikey" => "apikey",
        "codex" => "codex",
        "grok" => "grok",
        _ => "unknown",
    }
}

fn window_from_doc(doc: &Option<WindowDoc>) -> Option<QuotaWindow> {
    doc.as_ref().map(|w| QuotaWindow {
        utilization: w.utilization,
        resets_at: secs_time(w.resets_at),
        fetched_at: ms_time(w.fetched_at_ms),
        source: match w.source.as_str() {
            "poll" => WindowSource::UsagePoll,
            _ => WindowSource::Headers,
        },
    })
}

impl DashboardView {
    pub(crate) fn from_doc(doc: &DashboardDoc) -> Self {
        // Reconstruction timestamp for scoped windows: the document's scoped
        // shape ([`crate::dashboard::ScopedWindowDoc`]) deliberately omits
        // `fetched_at`/`source` (it is a convenience read-surface, not a
        // scheduler-reconstruction input), so a rebuilt `ScopedQuotaWindow`
        // carries no upstream fetch instant. Stamp it with "received now": the
        // Fbl gauge's freshness reads as populated (it never gates scheduling),
        // and its poll-degraded state still surfaces from the poller health.
        let received_at = SystemTime::now();
        let accounts: Vec<AccountSnapshot> = doc
            .accounts
            .iter()
            .map(|a| AccountSnapshot {
                id: AccountId(a.name.clone()),
                healthy: a.healthy,
                credential_kind: kind_static(&a.kind),
                group: crate::routing::BackendGroup::from_kind(kind_static(&a.kind)),
                five_hour: window_from_doc(&a.five_hour),
                seven_day: window_from_doc(&a.seven_day),
                // Scoped (`limits[]`) windows are rebuilt from the document so
                // the shared renderer's `account.fable_weekly()` resolves in
                // BOTH backends (local + attach) — the Fbl gauge (W3) reads it.
                // The doc's scoped shape omits `fetched_at`/`source` (see
                // `received_at` above), so those are synthesized; utilization /
                // reset / severity / is_active — everything the gauge shows —
                // round-trip faithfully.
                scoped_limits: a
                    .scoped_limits
                    .iter()
                    .map(|s| ScopedQuotaWindow {
                        scope_label: s.scope_label.clone(),
                        window: QuotaWindow {
                            utilization: s.window.utilization,
                            resets_at: secs_time(s.window.resets_at),
                            fetched_at: received_at,
                            source: WindowSource::UsagePoll,
                        },
                        severity: LimitSeverity::from_label(&s.window.severity),
                        is_active: s.window.is_active,
                    })
                    .collect(),
                // Scoped cooldowns are a live daemon-side concept (fable-usage
                // W2); the reconstructed-from-doc snapshot never gates requests,
                // so it carries none.
                scoped_cooldowns: Vec::new(),
                cooldown_until: a.cooldown_until.map(secs_time),
                cooldown_source: a.cooldown_source.as_deref().map(|s| match s {
                    "retry_after" => CooldownSource::RetryAfter,
                    _ => CooldownSource::Heuristic,
                }),
                in_flight: a.in_flight,
                token_expires_at_ms: a.token_expires_at_ms,
                last_refresh_ms: a.last_refresh_ms,
                paused: a.paused,
                limits: a.limits.unwrap_or_default(),
            })
            .collect();
        // Rebuild the per-group current map. A current daemon sends the full
        // per-group map (`current_by_group`), so each group's sticky slot
        // renders independently (req1). Fall back to the representative scalar
        // (`current`) — placed into its own group's slot — for docs from an
        // older daemon that predates the map.
        let mut current = std::collections::BTreeMap::new();
        if !doc.current_by_group.is_empty() {
            for (label, name) in &doc.current_by_group {
                current.insert(
                    crate::routing::BackendGroup::from_label(label),
                    AccountId(name.clone()),
                );
            }
        } else if let Some(name) = &doc.current {
            let id = AccountId(name.clone());
            let group = accounts
                .iter()
                .find(|a| a.id == id)
                .map(|a| a.group)
                .unwrap_or(crate::routing::BackendGroup::Claude);
            current.insert(group, id);
        }
        let snapshot = PoolSnapshot {
            accounts,
            current,
            // The TUI reconstructs the snapshot from the status doc for
            // rendering; the fable-scope current is a daemon-side scheduling
            // slot not carried in the doc, so it is empty here (display marks
            // the non-Fable current; the churn fix lives in the daemon).
            fable_current: std::collections::BTreeMap::new(),
            manual_pin: Default::default(),
        };
        let session_totals: HashMap<String, Totals> = doc
            .accounts
            .iter()
            .map(|a| {
                (
                    a.name.clone(),
                    Totals {
                        requests: a.session.requests,
                        ok: a.session.ok,
                        errors: a.session.errors,
                        tokens_in: a.session.tokens_in,
                        tokens_out: a.session.tokens_out,
                    },
                )
            })
            .collect();
        let poll_health: HashMap<String, PollHealth> = doc
            .poller
            .iter()
            .map(|p| {
                (
                    p.account.clone(),
                    PollHealth {
                        last_ok: p.last_ok_ms.map(ms_time),
                        consecutive_failures: p.consecutive_failures,
                        next_at: ms_time(p.next_at_ms),
                    },
                )
            })
            .collect();
        let in_flight = doc
            .activity
            .in_flight
            .iter()
            .map(|r| InFlight {
                id: r.id,
                method: r.method.clone(),
                path: r.path.clone(),
                account: r.account.clone(),
                // group/model/effort/fast are filled at routing time and
                // carried over the wire so the in-flight row shows the same
                // metadata badge as a completed row while running (issue #2 2a).
                group: r.group.clone(),
                model: r.model.clone(),
                effort: r.effort.clone(),
                fast: r.fast,
                // Kind rides the wire too (TUI UI-6 item 1) so the attached
                // in-flight row shows the same aligned `kind` column.
                kind: r.kind.clone(),
                started_at: ms_time(r.started_at_ms),
            })
            .collect();
        let completed = doc
            .activity
            .completed
            .iter()
            .map(|entry| match entry {
                CompletedDoc::Request {
                    id,
                    at_ms,
                    method,
                    path,
                    account,
                    status,
                    duration_ms,
                    tokens,
                    group,
                    model,
                    effort,
                    fast,
                    ttfb_ms,
                    ttft_ms,
                    gen_ms,
                    aborted,
                    user_id,
                    msg_kind,
                    excerpt,
                    // Per-request cost is carried in the doc for downstream
                    // consumers (server.log, JSON); the in-process view-model
                    // does not surface it — ui.rs reads the doc field directly.
                    cost_usd: _,
                } => Completed {
                    at: ms_time(*at_ms),
                    body: CompletedBody::Request {
                        id: *id,
                        method: method.clone(),
                        path: path.clone(),
                        account: account.clone(),
                        status: *status,
                        duration: Duration::from_millis(*duration_ms),
                        // Full token split, cache counters included, so the
                        // ATTACH-mode detail row renders cache_read /
                        // cache_creation instead of a permanent `—`. `None`
                        // stays `None` (older docs / upstream didn't report).
                        tokens: tokens.map(|t| TokenCounts {
                            input: t.input,
                            output: t.output,
                            cache_read: t.cache_read,
                            cache_creation: t.cache_creation,
                        }),
                        group: group.clone(),
                        model: model.clone(),
                        effort: effort.clone(),
                        fast: *fast,
                        ttfb_ms: *ttfb_ms,
                        ttft_ms: *ttft_ms,
                        gen_ms: *gen_ms,
                        aborted: *aborted,
                        user_id: user_id.clone(),
                        kind: msg_kind.clone(),
                        excerpt: excerpt.clone(),
                    },
                },
                CompletedDoc::Note { at_ms, text, error } => Completed {
                    at: ms_time(*at_ms),
                    body: CompletedBody::Note {
                        text: text.clone(),
                        error: *error,
                    },
                },
            })
            .collect();
        let logs = doc
            .logs
            .iter()
            .map(|line| LogLine {
                level: tracing::Level::from_str(&line.level).unwrap_or(tracing::Level::INFO),
                text: line.text.clone(),
            })
            .collect();

        Self {
            session_labels: doc.session_labels.clone(),
            grok: doc.grok.clone(),
            daily_usage: doc.daily_usage.clone(),
            daily_perf: doc.daily_perf.clone(),
            config_facts: doc.config_facts.clone(),
            usage_stats: doc.usage_stats.clone(),
            version: doc.version.clone(),
            pid: doc.pid,
            uptime: Duration::from_secs(doc.uptime_secs),
            port: doc.port,
            upstream: Some(doc.upstream.clone()).filter(|u: &String| !u.is_empty()),
            config_path: doc.config_path.clone(),
            select_params: SelectParams::from(&doc.select_params),
            refresh_ahead: Duration::from_secs(doc.refresh_ahead_secs),
            evaluate_tick: Duration::from_secs(doc.evaluate_tick_secs.max(1)),
            snapshot,
            last_switch: doc.scheduler.last_switch.as_ref().map(|s| LastSwitch {
                from: s.from.clone(),
                to: s.to.clone(),
                reason: s.reason.clone(),
                at: ms_time(s.at_ms),
            }),
            poll_health,
            session_totals,
            global_totals: Totals {
                requests: doc.totals.requests,
                ok: doc.totals.ok,
                errors: doc.totals.errors,
                tokens_in: doc.totals.tokens_in,
                tokens_out: doc.totals.tokens_out,
            },
            rpm_5m: doc.totals.rpm_5m,
            in_flight,
            completed,
            logs,
            model_usage: doc.model_usage.clone(),
            client_usage: doc.client_usage.clone(),
            windowed: doc.windowed.clone(),
            codex: doc.codex.clone(),
            email_anonymous: doc.email_anonymous,
            tui_effects: doc.tui_effects,
            gradient: crate::tui::ui::GradientCfg::from_config(&doc.tui_gradient),
            show_fable_weekly: doc.show_fable_weekly,
            domain_abbrev: doc.domain_abbrev.clone(),
            quota_display: doc.quota_display,
            data_quality: doc.data_quality.clone(),
            // Carried on the wire from the daemon's live event holder (config
            // `events`), so the banner renders identically in BOTH backends and
            // a `POST /llmux/events` reflects on the next document. Absent →
            // empty (no banner).
            events: doc.events.clone(),
            health: doc.health.as_ref().map(|h| super::activity::HealthCounts {
                requests: h.requests,
                errors: h.errors,
                s429: h.s429,
                s401: h.s401,
                s5xx: h.s5xx,
            }),
        }
    }

    /// Display order of the accounts table: indices into `snapshot.accounts`
    /// in INTERVENTION order (glance-triage atom 2) — exhausted, then
    /// auth-broken, then known usage descending, then ready, then paused,
    /// then cold/unknown; stable (config index) within a tier. The same list
    /// drives the render AND every row cursor (switch/remove/limits), so a
    /// click can never mis-target.
    pub(crate) fn display_order(&self, now: SystemTime) -> Vec<usize> {
        super::triage::intervention_order(&self.snapshot, &self.select_params, now)
    }

    pub(crate) fn totals_for(&self, account: &str) -> Totals {
        self.session_totals
            .get(account)
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn poll_health(&self, account: &str) -> Option<PollHealth> {
        self.poll_health.get(account).copied()
    }

    /// "0.1.0 (channel id)" — the version string with the binary name
    /// stripped (the header already says whose version it is).
    pub(crate) fn display_version(&self) -> &str {
        self.version.strip_prefix("llmux ").unwrap_or(&self.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_json() -> serde_json::Value {
        serde_json::json!({
            "version": "llmux 0.1.0 (dev dev)",
            "pid": 61282,
            "uptime_secs": 7980,
            "port": 3456,
            "current": "a",
            "upstream": "https://api.anthropic.com",
            "config_path": "/home/u/.config/llmux/llmux.json",
            "select_params": { "five_hour_max": 0.90, "seven_day_max": 0.99, "usage_max_age_secs": 600 },
            "refresh_ahead_secs": 25200,
            "evaluate_tick_secs": 60,
            "accounts": [
                {
                    "name": "a", "type": "oauth", "status": "active", "order": 1,
                    "blocked": null, "healthy": true,
                    "five_hour": { "utilization": 0.42, "resets_at": 1_003_600u64,
                                   "resets_in_secs": 3600, "fetched_at_ms": 1_000_000_000u64,
                                   "source": "headers" },
                    "seven_day": null,
                    "cooldown_until": null, "cooldown_source": null,
                    "in_flight": 1,
                    "token_expires_at_ms": 1_003_600_000u64, "last_refresh_ms": 999_820_000u64,
                    "totals": { "requests": 3, "input_tokens": 100, "output_tokens": 50 },
                    "session": { "requests": 3, "ok": 2, "errors": 1, "tokens_in": 100, "tokens_out": 50 },
                },
                {
                    "name": "b", "type": "apikey", "status": "cooldown", "order": 2,
                    "blocked": "cooldown 2m00s", "healthy": true,
                    "five_hour": null, "seven_day": null,
                    "cooldown_until": 1_000_120u64, "cooldown_source": "retry_after",
                    "in_flight": 0,
                    "token_expires_at_ms": null, "last_refresh_ms": null,
                    "totals": { "requests": 0, "input_tokens": 0, "output_tokens": 0 },
                    "session": { "requests": 0, "ok": 0, "errors": 0, "tokens_in": 0, "tokens_out": 0 },
                },
            ],
            "scheduler": {
                "last_switch": { "from": null, "to": "a", "reason": "initial selection",
                                 "at_ms": 999_910_000u64 },
                "next_in_line": null,
                "next_eval_in_secs": 42,
            },
            "poller": [
                { "account": "a", "last_ok_ms": 999_990_000u64, "consecutive_failures": 0,
                  "next_at_ms": 1_000_290_000u64 },
            ],
            "totals": { "requests": 3, "ok": 2, "errors": 1, "tokens_in": 100,
                        "tokens_out": 50, "rpm_5m": 0.6, "in_flight": 1 },
            "model_usage": [
                { "group": "claude", "model": "claude-sonnet-4-5", "requests": 3,
                  "ok": 2, "errors": 1, "tokens_in": 100, "tokens_out": 50,
                  "cache_read": 4000, "last_used_ms": 999_940_000u64, "in_flight": 1,
                  "accounts": [ { "name": "a", "requests": 3, "ok": 2, "errors": 1,
                                  "tokens_in": 100, "tokens_out": 50 } ],
                  "efforts": [ { "label": "16k", "requests": 1 },
                               { "label": "none", "requests": 2 } ],
                  "endpoints": [ { "label": "messages", "requests": 3 } ] },
            ],
            "windowed": [
                { "window": "24h", "window_secs": 86400,
                  "cells": [
                    { "group": "claude", "model": "claude-sonnet-4-5", "account": "a",
                      "requests": 3, "ok": 2, "errors": 1, "tokens_in": 100,
                      "tokens_out": 50, "cache_read": 4000, "cache_creation": 0,
                      "tokens": 4150 } ] },
                { "window": "72h", "window_secs": 259200, "cells": [] },
            ],
            "activity": {
                "in_flight": [
                    { "id": 7, "method": "POST", "path": "/v1/messages", "account": "a",
                      "started_at_ms": 999_997_000u64 },
                ],
                "completed": [
                    { "kind": "request", "at_ms": 999_940_000u64, "method": "POST",
                      "path": "/v1/messages", "account": "a", "status": 200,
                      "duration_ms": 1400,
                      "tokens": { "input": 70, "output": 30, "cache_read": 12 } },
                    { "kind": "note", "at_ms": 999_910_000u64,
                      "text": "switch (none) → a (initial selection)", "error": false },
                ],
            },
            "logs": [
                { "level": "INFO", "text": "proxy: proxy listening" },
                { "level": "ERROR", "text": "refresh: token dead" },
            ],
        })
    }

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_000_000)
    }

    #[test]
    fn from_doc_rebuilds_per_group_current_from_map() {
        use crate::routing::BackendGroup;
        let mut json = doc_json();
        // A codex account joins the roster, and the doc carries a per-group
        // current map with BOTH slots (what a current daemon emits).
        json["accounts"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "name": "c", "type": "codex", "status": "active", "order": 3,
                "blocked": null, "healthy": true,
                "five_hour": null, "seven_day": null,
                "cooldown_until": null, "cooldown_source": null,
                "in_flight": 0,
                "token_expires_at_ms": null, "last_refresh_ms": null,
                "totals": { "requests": 0, "input_tokens": 0, "output_tokens": 0 },
                "session": { "requests": 0, "ok": 0, "errors": 0, "tokens_in": 0, "tokens_out": 0 },
            }));
        json["current_by_group"] = serde_json::json!({ "claude": "a", "codex": "c" });

        let doc: DashboardDoc = serde_json::from_value(json).expect("parse doc");
        let view = DashboardView::from_doc(&doc);

        assert_eq!(
            view.snapshot.current_for_group(BackendGroup::Claude),
            Some(&AccountId("a".into()))
        );
        assert_eq!(
            view.snapshot.current_for_group(BackendGroup::Codex),
            Some(&AccountId("c".into()))
        );
        assert!(view.snapshot.is_current(&AccountId("c".into())));
    }

    #[test]
    fn from_doc_carries_in_flight_group_and_model_for_the_badge() {
        // Regression (issue #2 2a): the in-flight model badge never rendered
        // because InFlightDoc dropped group/model on the wire and from_doc
        // hardcoded None. Assert they now survive the HubDoc→JSON→from_doc
        // round-trip the real `dashboard` uses (the unit test that constructs
        // InFlight directly bypassed exactly this hop).
        let mut json = doc_json();
        let infl = &mut json["activity"]["in_flight"][0];
        infl["group"] = serde_json::json!("claude");
        infl["model"] = serde_json::json!("claude-opus-4-8");
        infl["effort"] = serde_json::json!("low");
        infl["fast"] = serde_json::json!(true);

        let doc: DashboardDoc = serde_json::from_value(json).expect("parse doc");
        let view = DashboardView::from_doc(&doc);

        assert_eq!(view.in_flight.len(), 1);
        assert_eq!(view.in_flight[0].group.as_deref(), Some("claude"));
        assert_eq!(view.in_flight[0].model.as_deref(), Some("claude-opus-4-8"));
        // effort/fast ride the same hop so the running badge matches the
        // completed badge.
        assert_eq!(view.in_flight[0].effort.as_deref(), Some("low"));
        assert!(view.in_flight[0].fast);

        // And a doc WITHOUT the fields still parses (back-compat → None/false).
        let doc2: DashboardDoc = serde_json::from_value(doc_json()).expect("parse legacy doc");
        let view2 = DashboardView::from_doc(&doc2);
        assert_eq!(view2.in_flight[0].group, None);
        assert_eq!(view2.in_flight[0].model, None);
        assert_eq!(view2.in_flight[0].effort, None);
        assert!(!view2.in_flight[0].fast);
    }

    #[test]
    fn from_doc_round_trips_the_event_banners() {
        // The event banners ride the doc from the daemon's live holder, so both
        // backends render them through from_doc. Present → carried verbatim.
        let mut json = doc_json();
        json["events"] = serde_json::json!([{
            "id": "20260712-fable5",
            "from": "202607080000",
            "to": "202607130000",
            "content": "Fable 5 Available until 7/12",
        }]);
        let doc: DashboardDoc = serde_json::from_value(json).expect("parse doc");
        let view = DashboardView::from_doc(&doc);
        assert_eq!(view.events.len(), 1, "events carried through from_doc");
        assert_eq!(view.events[0].id, "20260712-fable5");
        assert_eq!(view.events[0].content, "Fable 5 Available until 7/12");

        // Absent (older/quiet daemon) → parses to empty, no banner. This is the
        // additive `skip_serializing_if` contract: `doc_json()` has no `events`.
        let doc2: DashboardDoc = serde_json::from_value(doc_json()).expect("parse legacy doc");
        assert!(DashboardView::from_doc(&doc2).events.is_empty());
    }

    #[test]
    fn from_doc_falls_back_to_scalar_current_when_map_absent() {
        use crate::routing::BackendGroup;
        // Legacy daemon: no current_by_group, only the scalar `current`.
        let doc: DashboardDoc = serde_json::from_value(doc_json()).expect("parse doc");
        assert!(doc.current_by_group.is_empty());
        let view = DashboardView::from_doc(&doc);
        // "a" is oauth (claude) → lands in the claude slot; codex stays empty.
        assert_eq!(
            view.snapshot.current_for_group(BackendGroup::Claude),
            Some(&AccountId("a".into()))
        );
        assert_eq!(view.snapshot.current_for_group(BackendGroup::Codex), None);
    }

    #[test]
    fn view_model_builds_from_fetched_json() {
        let doc: DashboardDoc = serde_json::from_value(doc_json()).expect("parse doc");
        let view = DashboardView::from_doc(&doc);

        assert_eq!(view.pid, 61282);
        assert_eq!(view.port, 3456);
        assert_eq!(view.uptime, Duration::from_secs(7980));
        assert_eq!(view.display_version(), "0.1.0 (dev dev)");
        assert_eq!(
            view.snapshot.representative_current(),
            Some(&AccountId("a".into()))
        );

        let a = &view.snapshot.accounts[0];
        assert_eq!(a.credential_kind, "oauth");
        assert!(a.healthy);
        let five = a.five_hour.expect("window");
        assert!((five.utilization - 0.42).abs() < 1e-9);
        assert_eq!(five.resets_at, UNIX_EPOCH + Duration::from_secs(1_003_600));
        assert_eq!(five.fetched_at, now());
        assert_eq!(five.source, WindowSource::Headers);
        assert_eq!(a.token_expires_at_ms, Some(1_003_600_000));
        assert_eq!(a.in_flight, 1);

        let b = &view.snapshot.accounts[1];
        assert_eq!(b.credential_kind, "apikey");
        assert_eq!(
            b.cooldown_until,
            Some(UNIX_EPOCH + Duration::from_secs(1_000_120))
        );
        assert_eq!(b.cooldown_source, Some(CooldownSource::RetryAfter));

        // The pure scheduler functions run on the rebuilt snapshot: the
        // parked account gates exactly like it does server-side.
        assert_eq!(
            crate::scheduler::select::eligibility(b, &view.select_params, now(), false),
            Some(crate::scheduler::select::IneligibleReason::CoolingDown)
        );
        assert_eq!(view.display_order(now()), vec![0, 1]);

        assert_eq!(view.totals_for("a").ok, 2);
        assert_eq!(view.global_totals.errors, 1);
        assert!((view.rpm_5m - 0.6).abs() < 1e-9);

        let poll = view.poll_health("a").expect("poll health");
        assert_eq!(poll.consecutive_failures, 0);
        assert_eq!(
            poll.last_ok,
            Some(UNIX_EPOCH + Duration::from_millis(999_990_000))
        );

        assert_eq!(view.in_flight.len(), 1);
        assert_eq!(view.in_flight[0].account.as_deref(), Some("a"));
        assert_eq!(view.completed.len(), 2);
        match &view.completed[0].body {
            CompletedBody::Request {
                status,
                duration,
                tokens,
                ..
            } => {
                assert_eq!(*status, 200);
                assert_eq!(*duration, Duration::from_millis(1400));
                // The cache split survives doc→view (ATTACH mode): a reported
                // cache_read arrives; an unreported cache_creation stays None.
                assert_eq!(
                    *tokens,
                    Some(TokenCounts {
                        input: 70,
                        output: 30,
                        cache_read: Some(12),
                        cache_creation: None,
                    })
                );
            }
            other => panic!("expected request, got {other:?}"),
        }
        assert_eq!(view.logs.len(), 2);
        assert_eq!(view.logs[1].level, tracing::Level::ERROR);

        let switch = view.last_switch.expect("last switch");
        assert_eq!(switch.to, "a");
        assert_eq!(switch.from, None);
    }

    #[test]
    fn model_usage_survives_doc_to_view_without_loss() {
        // Local and attach both go through from_doc, so a row produced by the
        // document builder must reach the renderer input intact (req21/31).
        let doc: DashboardDoc = serde_json::from_value(doc_json()).expect("parse doc");
        let view = DashboardView::from_doc(&doc);
        assert_eq!(view.model_usage.len(), 1);
        let row = &view.model_usage[0];
        assert_eq!(row.group, "claude");
        assert_eq!(row.model, "claude-sonnet-4-5");
        assert_eq!(row.tokens_in, 100);
        assert_eq!(row.tokens_out, 50);
        assert_eq!(row.cache_read, Some(4000));
        assert_eq!(row.cache_creation, None);
        assert_eq!(row.in_flight, 1);
        assert_eq!(row.accounts.len(), 1);
        assert_eq!(row.efforts.len(), 2);
        assert_eq!(row.endpoints[0].label, "messages");
    }

    #[test]
    fn model_usage_defaults_to_empty_for_older_documents() {
        let mut value = doc_json();
        value.as_object_mut().unwrap().remove("model_usage");
        let doc: DashboardDoc = serde_json::from_value(value).expect("parse doc");
        let view = DashboardView::from_doc(&doc);
        assert!(view.model_usage.is_empty());
    }

    #[test]
    fn data_quality_labels_reach_the_view_with_canonical_defaults() {
        // doc_json() predates `data_quality` (issue #62 S2): the serde default
        // fills the canonical wording and from_doc carries it verbatim, so the
        // renderer shows identical labels for old and new daemons.
        let doc: DashboardDoc = serde_json::from_value(doc_json()).expect("parse doc");
        let view = DashboardView::from_doc(&doc);
        assert_eq!(view.data_quality.model_usage, "hydrated activity/runtime");
        assert_eq!(view.data_quality.cost, "API-equivalent estimate");
    }

    #[test]
    fn windowed_heatmap_survives_doc_to_view_without_loss() {
        // Local and attach both go through from_doc (issue #23), so the windowed
        // slices must reach the renderer input intact.
        let doc: DashboardDoc = serde_json::from_value(doc_json()).expect("parse doc");
        let view = DashboardView::from_doc(&doc);
        assert_eq!(view.windowed.len(), 2);
        let day = view
            .windowed
            .iter()
            .find(|w| w.window == "24h")
            .expect("24h slice");
        assert_eq!(day.cells.len(), 1);
        let cell = &day.cells[0];
        assert_eq!(cell.group, "claude");
        assert_eq!(cell.model, "claude-sonnet-4-5");
        assert_eq!(cell.account, "a");
        assert_eq!(cell.requests, 3);
        assert_eq!(cell.tokens, 4150);
        // The 72h slice is present but empty in this fixture.
        let three = view
            .windowed
            .iter()
            .find(|w| w.window == "72h")
            .expect("72h slice");
        assert!(three.cells.is_empty());
    }

    #[test]
    fn windowed_defaults_to_empty_for_older_documents() {
        let mut value = doc_json();
        value.as_object_mut().unwrap().remove("windowed");
        let doc: DashboardDoc = serde_json::from_value(value).expect("parse doc");
        let view = DashboardView::from_doc(&doc);
        assert!(view.windowed.is_empty());
    }

    #[test]
    fn email_anonymous_flag_survives_doc_to_view_and_defaults_off() {
        // A doc from an older daemon (no field) → masking off (E7 server-side
        // analogue); a current daemon's flag reaches the renderer input.
        let doc: DashboardDoc = serde_json::from_value(doc_json()).expect("parse doc");
        let view = DashboardView::from_doc(&doc);
        assert!(!view.email_anonymous, "absent field defaults off");

        let mut json = doc_json();
        json["email_anonymous"] = serde_json::json!(true);
        let doc: DashboardDoc = serde_json::from_value(json).expect("parse doc");
        let view = DashboardView::from_doc(&doc);
        assert!(view.email_anonymous);
        // The view snapshot keeps REAL ids — interactions (switch/remove)
        // address the pool by real name; masking is draw-time only.
        assert_eq!(view.snapshot.accounts[0].id.0, "a");
    }

    #[test]
    fn fable_weekly_survives_doc_to_view_and_toggle_defaults_on() {
        // A daemon that emits the scoped Fable window must reach the renderer
        // input intact through from_doc — `account.fable_weekly()` is what the
        // Fbl gauge (W3) reads, in BOTH backends. And an older daemon's doc with
        // no `show_fable_weekly` field defaults the gauge ON.
        let mut json = doc_json();
        json["accounts"][0]["fable_weekly"] = serde_json::json!({
            "utilization": 0.97, "resets_at": 1_600_000u64, "resets_in_secs": 600_000u64,
            "severity": "critical", "is_active": true,
        });
        json["accounts"][0]["scoped_limits"] = serde_json::json!([{
            "scope_label": "Fable", "utilization": 0.97, "resets_at": 1_600_000u64,
            "resets_in_secs": 600_000u64, "severity": "critical", "is_active": true,
        }]);
        let doc: DashboardDoc = serde_json::from_value(json).expect("parse doc");
        // Absent field → default ON.
        assert!(doc.show_fable_weekly, "absent wire field defaults ON");
        let view = DashboardView::from_doc(&doc);
        assert!(view.show_fable_weekly);
        let fable = view.snapshot.accounts[0]
            .fable_weekly()
            .expect("Fable scope rebuilt from doc");
        assert!((fable.window.utilization - 0.97).abs() < 1e-9);
        assert_eq!(fable.severity, LimitSeverity::Critical);
        assert!(fable.is_active);

        // An explicit `false` on the wire threads through to the view.
        let mut json = doc_json();
        json["show_fable_weekly"] = serde_json::json!(false);
        let doc: DashboardDoc = serde_json::from_value(json).expect("parse doc");
        let view = DashboardView::from_doc(&doc);
        assert!(!view.show_fable_weekly);
        // No scoped rows in this doc → empty, not a crash.
        assert!(view.snapshot.accounts[0].fable_weekly().is_none());
    }

    #[test]
    fn display_settings_survive_doc_to_view_and_default() {
        // A doc from an older daemon (neither field) → the built-in abbrev map
        // and `used` fill, mirroring the show_fable_weekly additive convention.
        let doc: DashboardDoc = serde_json::from_value(doc_json()).expect("parse doc");
        let view = DashboardView::from_doc(&doc);
        assert_eq!(
            view.domain_abbrev
                .get("insightquest.io")
                .map(String::as_str),
            Some("iq.io"),
            "absent wire field defaults to the built-in map"
        );
        assert_eq!(
            view.quota_display,
            crate::config::QuotaDisplay::Remaining,
            "default fill is remaining — full green bar draining"
        );

        // Explicit wire values thread through.
        let mut json = doc_json();
        json["domain_abbrev"] = serde_json::json!({"example.com": "ex"});
        json["quota_display"] = serde_json::json!("used");
        let doc: DashboardDoc = serde_json::from_value(json).expect("parse doc");
        let view = DashboardView::from_doc(&doc);
        assert_eq!(
            view.domain_abbrev.get("example.com").map(String::as_str),
            Some("ex")
        );
        assert!(!view.domain_abbrev.contains_key("insightquest.io"));
        assert_eq!(view.quota_display, crate::config::QuotaDisplay::Used);
    }

    #[test]
    fn unknown_credential_kind_degrades_without_special_casing() {
        let mut doc: DashboardDoc = serde_json::from_value(doc_json()).expect("parse doc");
        doc.accounts[0].kind = "gemini".into();
        let view = DashboardView::from_doc(&doc);
        assert_eq!(view.snapshot.accounts[0].credential_kind, "unknown");
    }

    #[test]
    fn grok_credential_kind_maps_to_grok_group() {
        // Attach-mode reconstruction must carry a grok account's group through
        // to BackendGroup::Grok, not fall through "unknown" to Claude.
        let mut doc: DashboardDoc = serde_json::from_value(doc_json()).expect("parse doc");
        doc.accounts[0].kind = "grok".into();
        let view = DashboardView::from_doc(&doc);
        assert_eq!(view.snapshot.accounts[0].credential_kind, "grok");
        assert_eq!(
            view.snapshot.accounts[0].group,
            crate::routing::BackendGroup::Grok
        );
    }
}
