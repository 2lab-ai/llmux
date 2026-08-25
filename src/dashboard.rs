//! Dashboard state + document: the single source of truth behind both the
//! in-process TUI and the remote attach mode (`llmux dashboard`).
//!
//! - [`DashboardHub`] — server-owned fold of the activity-event stream and
//!   the tracing bridge: activity ring, per-account totals, last switch,
//!   poller health, log console. Lives in `proxy::server::AppState`; one
//!   fold task ([`fold`]) consumes the event/log channels into it.
//! - [`DashboardDoc`] — the serializable superset of `/llmux/status`
//!   served at `GET /llmux/dashboard`. The local TUI builds the SAME
//!   document in-process every frame and the remote client parses it from JSON,
//!   so both render paths share one contract (`tui::view` converts it into
//!   the view-model the draw code consumes).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::logging::LogLine;
use crate::proxy::server::{AppState, UsageTotals, EVALUATE_TICK};
use crate::scheduler::select::{self, SelectParams};
use crate::scheduler::{AccountSnapshot, CooldownSource, PoolSnapshot};
use crate::tui::activity::{
    normalize_model, ActivityLog, ClientUsage, Completed, CompletedBody, InFlight, ModelUsage,
    StatsWindow, Totals, WindowedRow,
};
use crate::tui::logs::LogConsole;
use crate::tui::{ActivityEvent, LastSwitch, PollHealth, TokenCounts};

/// Completed-activity entries served in the document. Matches the hub ring
/// ([`crate::tui::activity::LOG_CAPACITY`]) so the attach client can scroll
/// the FULL retained history (the activity panel is scrollable now), not just
/// a glance window. At a 1 Hz poll this is ~200 small JSON objects — cheap.
pub const ACTIVITY_TAIL: usize = 200;
/// Tracing lines served in the document.
pub const LOG_TAIL: usize = 100;
/// Trailing window for the requests-per-minute figure.
pub const RPM_WINDOW: Duration = Duration::from_secs(5 * 60);

// ---------------------------------------------------------------------------
// Hub: server-owned observability state
// ---------------------------------------------------------------------------

/// Server-side fold of activity events + tracing lines. All mutations are
/// sync and short (std Mutex, never held across an await) — same locking
/// discipline as the scheduler pool.
pub struct DashboardHub {
    inner: Mutex<HubState>,
}

struct HubState {
    log: ActivityLog,
    last_switch: Option<LastSwitch>,
    poll_health: HashMap<String, PollHealth>,
    console: LogConsole,
    /// Where finished requests are appended (req-persist A/C). `None` until the
    /// daemon arms persistence in [`DashboardHub::load_from_state_dir`]; left
    /// `None` in unit tests (which build the hub via `default()` and never call
    /// `serve`), so folding events through the hub never touches the real state
    /// dir during tests.
    persist_path: Option<std::path::PathBuf>,
}

impl Default for DashboardHub {
    fn default() -> Self {
        // Pure construction — NO filesystem IO. Tests build the hub via
        // `default()` and must start from an empty log; the persisted-log
        // replay is an explicit, daemon-only step ([`Self::load_from_state_dir`])
        // run once at serve startup, never on construction.
        Self {
            inner: Mutex::new(HubState {
                log: ActivityLog::new(crate::tui::activity::LOG_CAPACITY),
                last_switch: None,
                poll_health: HashMap::new(),
                console: LogConsole::new(crate::tui::logs::LOG_CONSOLE_CAPACITY),
                persist_path: None,
            }),
        }
    }
}

impl DashboardHub {
    fn lock(&self) -> std::sync::MutexGuard<'_, HubState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Arm persistence at `path` (req-persist A/C): record the append target so
    /// subsequent `apply_event`s persist finished requests. Deliberately does
    /// NOT read the file — resuming the persisted history is a separate,
    /// post-readiness step ([`Self::hydrate_persisted`]), so a large log can
    /// never delay the listener coming up.
    ///
    /// Returns the file's CURRENT byte length (0 when missing/`None`): the
    /// hydration cut. Everything before it is pre-boot history to replay;
    /// everything after it is live appends from this process — reading only up
    /// to the cut is what prevents double-counting a request that finished
    /// while history was still loading.
    ///
    /// `path` is `state.activity_log_path`: the state-dir `activity.jsonl` for a
    /// real daemon, a tempdir for e2e, `None` to disable. Called once from
    /// `serve` — never from `Default`, so unit tests that build the hub via
    /// `default()` and fold events stay isolated (their `persist_path` is
    /// `None`).
    pub fn arm_persistence(&self, path: Option<std::path::PathBuf>) -> u64 {
        let cut = path
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        self.lock().persist_path = path;
        cut
    }

    /// Resume the persisted activity history (req-persist A/C), background
    /// edition: replay the first `up_to` bytes of `path` (the cut returned by
    /// [`Self::arm_persistence`]) into a FRESH log OFF the hub lock, then merge
    /// it BEHIND whatever live traffic has already folded
    /// ([`ActivityLog::merge_history_behind`] — live rows stay in front, sums
    /// commute, windowed buckets land in their original hours, live in-flight
    /// rows are never touched).
    ///
    /// Blocking (file IO + parse) — call from `spawn_blocking`. The hub lock is
    /// held only for the in-memory merge. Degrades gracefully: a missing file
    /// or `up_to == 0` is a silent no-op (first boot); a read error starts with
    /// empty history and leaves a warning + an error note in the activity log,
    /// never a crash.
    pub fn hydrate_persisted(&self, path: Option<&std::path::Path>, up_to: u64) {
        let Some(path) = path else {
            return;
        };
        if up_to == 0 {
            return; // nothing persisted before this boot
        }
        let mut history = ActivityLog::new(crate::tui::activity::LOG_CAPACITY);
        let loaded = history.load_persisted_prefix(path, up_to);
        let now = SystemTime::now();
        let mut state = self.lock();
        match loaded {
            Ok(()) => {
                let merged = state.log.merge_history_behind(history);
                if merged > 0 {
                    state.log.push_note(
                        format!("history loaded: {merged} persisted requests resumed"),
                        false,
                        now,
                    );
                }
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "activity history load failed");
                state.log.push_note(
                    format!("history load failed ({err}) — starting with empty history"),
                    true,
                    now,
                );
            }
        }
    }

    /// Fold one proxy/scheduler event: last-switch + poller-health pane
    /// state, then the activity log itself.
    pub fn apply_event(&self, event: ActivityEvent, now: SystemTime) {
        let mut state = self.lock();
        match &event {
            ActivityEvent::AccountSwitched { from, to, reason } => {
                state.last_switch = Some(LastSwitch {
                    from: from.clone(),
                    to: to.clone(),
                    reason: reason.clone(),
                    at: now,
                });
            }
            ActivityEvent::UsagePolled {
                account,
                ok,
                consecutive_failures,
                next_in,
            } => {
                let entry = state
                    .poll_health
                    .entry(account.clone())
                    .or_insert(PollHealth {
                        last_ok: None,
                        consecutive_failures: 0,
                        next_at: now,
                    });
                if *ok {
                    entry.last_ok = Some(now);
                }
                entry.consecutive_failures = *consecutive_failures;
                entry.next_at = now + *next_in;
            }
            _ => {}
        }
        // Persist finished requests before folding (req-persist A/C). Borrow,
        // not move: `apply` still consumes the event below. The append target
        // is `None` until the daemon armed persistence in `load_from_state_dir`,
        // so this is a no-op in unit tests. Best-effort — a persistence failure
        // must never break the fold (and a non-finished event is a no-op inside
        // `persist_request`).
        if let Some(path) = state.persist_path.clone() {
            crate::tui::activity::persist_request(Some(&path), &event, now);
        }
        state.log.apply(event, now);
    }

    /// Append a raw tracing line to the log console ring.
    pub fn push_log(&self, line: LogLine) {
        self.lock().console.push(line);
    }

    /// Append an operator note ("config reloaded", …) to the activity log.
    pub fn push_note(&self, text: String, error: bool, now: SystemTime) {
        self.lock().log.push_note(text, error, now);
    }

    /// Point-in-time clone of everything the dashboard document needs.
    pub(crate) fn view(&self, now: SystemTime) -> HubView {
        let mut state = self.lock();
        // Sweep leaked in-flight rows on every read so the dashboard reflects
        // the daemon's real `in_flight` even when a `RequestFinished` event was
        // dropped on a full activity channel (BUG: zombie 25,000s+ rows).
        state.log.prune_stale_in_flight(now);
        HubView {
            last_switch: state.last_switch.clone(),
            poll_health: state.poll_health.clone(),
            health: state.log.health_counts(now),
            in_flight: state.log.in_flight().to_vec(),
            completed: state.log.completed().take(ACTIVITY_TAIL).cloned().collect(),
            account_totals: state.log.totals_map(),
            global_totals: state.log.totals_global(),
            rpm_5m: state.log.requests_per_minute(now, RPM_WINDOW),
            model_usage: state.log.model_usage(),
            client_usage: state.log.client_usage(),
            tenant_stats: state.log.tenant_stats().clone(),
            // Windowed heatmap rows per window (issue #23). Computed under the
            // same lock as the rest of the view so one read is consistent.
            windowed: StatsWindow::ALL
                .iter()
                .map(|&w| (w, state.log.windowed_rows(w, now)))
                .collect(),
            logs: state.console.tail(LOG_TAIL).cloned().collect(),
            session_labels: state.log.session_labels(),
            daily_usage: state.log.daily_usage(),
            daily_perf: state.log.daily_perf(),
            usage_stats: state.log.usage_stats(now),
        }
    }
}

/// Cloned hub state for one document build (no lock held while rendering).
pub(crate) struct HubView {
    pub last_switch: Option<LastSwitch>,
    pub poll_health: HashMap<String, PollHealth>,
    /// Rolling 5-minute status-class counts for the header health verdict
    /// (glance-triage). From the log's dedicated sample deque, NOT the
    /// capacity-bounded completed ring.
    pub health: super::tui::activity::HealthCounts,
    pub in_flight: Vec<InFlight>,
    /// Newest first (activity renders newest-top).
    pub completed: Vec<Completed>,
    pub account_totals: HashMap<String, Totals>,
    pub global_totals: Totals,
    pub rpm_5m: f64,
    /// Aggregated per-(group, model) usage rows, sorted by total tokens desc.
    pub model_usage: Vec<ModelUsage>,
    /// Per-client request attribution rows (issue #32), sorted by requests desc.
    pub client_usage: Vec<ClientUsage>,
    /// Per-TENANT aggregates (multi-tenant #22), keyed by attribution id
    /// (`k-…` / `legacy` / `local` / `unknown`): counts + per-model token
    /// sums + first/last-seen span. Priced into the doc at build time.
    pub tenant_stats: std::collections::BTreeMap<String, super::tui::activity::TenantStats>,
    /// Windowed heatmap rows per window (issue #23): one `(window, rows)` pair
    /// per [`StatsWindow`], each already sorted by total tokens desc.
    pub windowed: Vec<(StatsWindow, Vec<WindowedRow>)>,
    /// Oldest→newest (console renders the tail at the bottom).
    pub logs: Vec<LogLine>,
    /// Derived session titles (TUI UI-3 U2): client `user_id` → first
    /// user-input excerpt.
    pub session_labels: HashMap<String, String>,
    /// Tokens-per-day chart rows (UI-3 U14).
    pub daily_usage: Vec<DailyUsageDoc>,
    /// Observed-performance rows (perf telemetry v1).
    pub daily_perf: Vec<DailyPerfDoc>,
    /// Usage-tab calendar rows (usage-stats), cost not yet priced (the doc
    /// build adds it — pricing overrides live in the app state, not the log).
    pub usage_stats: Vec<UsageStatDoc>,
}

/// Consume the activity-event and tracing-line channels into the hub. The
/// single consumer of both streams; spawned next to the listener in
/// `proxy::server::serve` and aborted on shutdown. With `trace_events` each
/// activity event is also rendered as a tracing line (daemon mode: keeps
/// `server.log` carrying the request history the TUI would have shown).
pub async fn fold(
    hub: std::sync::Arc<DashboardHub>,
    mut events: tokio::sync::mpsc::Receiver<ActivityEvent>,
    logs: Option<tokio::sync::mpsc::Receiver<LogLine>>,
    trace_events: bool,
) {
    let mut logs_open = logs.is_some();
    let mut logs = logs;
    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Some(event) => {
                        if trace_events {
                            trace_event(&event);
                        }
                        hub.apply_event(event, SystemTime::now());
                    }
                    None => return, // every sender gone — server is down
                }
            }
            line = async {
                match logs.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            }, if logs_open => {
                match line {
                    Some(line) => hub.push_log(line),
                    None => logs_open = false,
                }
            }
        }
    }
}

/// Render one activity event as a tracing log line (daemon mode parity with
/// the old non-TTY event drain).
fn trace_event(event: &ActivityEvent) {
    match event {
        ActivityEvent::RequestStarted {
            id,
            method,
            path,
            kind,
        } => {
            tracing::debug!(
                id,
                %method,
                %path,
                kind = kind.as_deref().unwrap_or("-"),
                "request started"
            );
        }
        ActivityEvent::RequestRouted {
            id,
            account,
            group,
            model,
            effort,
            fast,
        } => {
            tracing::debug!(
                id,
                %account,
                group = group.as_deref().unwrap_or("-"),
                model = model.as_deref().unwrap_or("-"),
                effort = effort.as_deref().unwrap_or("-"),
                fast = *fast,
                "request routed"
            );
        }
        ActivityEvent::RequestFinished {
            id,
            method,
            path,
            account,
            status,
            duration,
            tokens,
            group,
            model,
            effort,
            fast,
            user_id,
            kind,
            excerpt: _,
            ttfb_ms: _,
            ttft_ms: _,
            gen_ms: _,
            aborted: _,
            tenant,
        } => {
            // API-equivalent USD cost for this request (Feature D). The fold
            // task has no config handle, so the log line uses the built-in
            // default rate table (empty overrides). 0.0 unless group, model,
            // and token usage are all known.
            let cost = match (group, model, tokens) {
                (Some(g), Some(m), Some(t)) => {
                    crate::pricing::cost_usd(g, m, t, &std::collections::HashMap::new())
                }
                _ => 0.0,
            };
            tracing::info!(
                id, %method, %path,
                account = account.as_deref().unwrap_or("-"),
                status,
                duration_ms = duration.as_millis() as u64,
                tokens = tokens.map(TokenCounts::total).unwrap_or(0),
                cost = format!("{cost:.4}"),
                group = group.as_deref().unwrap_or("-"),
                model = model.as_deref().unwrap_or("-"),
                effort = effort.as_deref().unwrap_or("-"),
                fast = fast.unwrap_or(false),
                client = user_id.as_deref().unwrap_or("unknown"),
                tenant = tenant.as_deref().unwrap_or("unknown"),
                kind = kind.as_deref().unwrap_or("-"),
                "request finished"
            );
        }
        ActivityEvent::AccountSwitched { from, to, reason } => {
            tracing::info!(
                from = from.as_deref().unwrap_or("(none)"),
                %to,
                reason = reason.as_deref().unwrap_or("-"),
                "account switched"
            );
        }
        ActivityEvent::TokenRefreshed {
            account,
            expires_at_ms,
        } => {
            tracing::info!(%account, expires_at_ms, "token refreshed");
        }
        ActivityEvent::UsagePolled {
            account,
            ok,
            consecutive_failures,
            next_in,
        } => {
            tracing::debug!(
                %account,
                ok,
                consecutive_failures,
                next_in_secs = next_in.as_secs(),
                "usage polled"
            );
        }
        ActivityEvent::Error { context, message } => {
            tracing::warn!(context = context.as_deref().unwrap_or("-"), %message, "proxy error");
        }
    }
}

// ---------------------------------------------------------------------------
// Document: the GET /llmux/dashboard contract
// ---------------------------------------------------------------------------

/// The `/llmux/dashboard` document — a strict superset of
/// `/llmux/status` (same account fields and ordering) plus scheduler /
/// poller / totals / activity / log state. Serialized by the server, parsed
/// by the attach-mode client, and built in-process by the local TUI — one
/// contract, one renderer. Fields are additive only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardDoc {
    pub version: String,
    pub pid: u32,
    pub uptime_secs: u64,
    pub port: u16,
    pub current: Option<String>,
    /// Per-group sticky current account (req1): `"claude"`/`"codex"` → account
    /// name, one entry per group that has a selection. The scalar `current`
    /// above stays the representative (claude slot first) for back-compat; this
    /// map drives the per-group `current` lines. Additive: docs written before
    /// this field default to an empty map and the renderer falls back to the
    /// scalar.
    #[serde(default)]
    pub current_by_group: BTreeMap<String, String>,
    pub upstream: String,
    pub config_path: Option<String>,
    pub select_params: SelectParamsDoc,
    pub refresh_ahead_secs: u64,
    pub evaluate_tick_secs: u64,
    /// Selection order (current → eligible by rank → ineligible), same as
    /// `/llmux/status`.
    pub accounts: Vec<AccountDoc>,
    pub scheduler: SchedulerDoc,
    pub poller: Vec<PollerDoc>,
    pub totals: GlobalTotalsDoc,
    /// Per-(group, served model) usage rows (req1-20). Additive: absent in docs
    /// written before this existed → an older client parses it as empty and an
    /// upgraded client attaching to an older daemon renders no model panel.
    #[serde(default)]
    pub model_usage: Vec<ModelUsageDoc>,
    /// Per-client request attribution (issue #32): in-memory request/token
    /// counts keyed by `metadata.user_id` (the `unknown` bucket holds
    /// requests with no id). Pure metering — no key issuance, no auth change.
    /// Additive: absent in docs written before this existed → an older client
    /// parses it as empty and an upgraded client attaching to an older daemon
    /// renders no client panel.
    #[serde(default)]
    pub client_usage: Vec<ClientUsageDoc>,
    /// Per-TENANT attribution rows (multi-tenant #22): keyed usage buckets
    /// resolved by the auth gate (`k-…` ids, `legacy`, `local`; `unknown` =
    /// pre-tenant history). Additive: absent in older docs → parses empty.
    #[serde(default)]
    pub tenant_usage: Vec<TenantUsageDoc>,
    /// Issued client keys (metadata ONLY — secrets are never stored, let
    /// alone serialized). Additive: absent in older docs → parses empty.
    #[serde(default)]
    pub client_keys: Vec<KeyRowDoc>,
    /// Windowed (24h/72h) per-account/per-model token heatmap rows (issue #23).
    /// Additive: absent in pre-#23 docs → an older client parses it empty and an
    /// upgraded client attaching to an older daemon shows no heatmap. These are
    /// a BEST-EFFORT sample (the activity event channel is lossy — events are
    /// `try_send` and dropped on a full channel), not an exact ledger.
    #[serde(default)]
    pub windowed: Vec<WindowedStatsDoc>,
    pub activity: ActivityDoc,
    /// Rolling 5-minute status-class counts for the header health verdict
    /// (glance-triage). Additive AND presence-preserving: `None` from an
    /// older daemon means "no telemetry", which the verdict renders as an
    /// unavailable err surface — never as a fabricated healthy 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthDoc>,
    /// Derived session titles (TUI UI-3 U2): client `user_id` → the first
    /// plain user-input excerpt seen for it. Additive: absent in older docs →
    /// empty map, rows just render without a session label.
    #[serde(default)]
    pub session_labels: BTreeMap<String, String>,
    /// Tracing tail, oldest→newest.
    pub logs: Vec<LogLineDoc>,
    /// Live codex request settings (req8.1 — dashboard fast/model/effort).
    /// Additive: absent in docs written before this existed.
    #[serde(default)]
    pub codex: CodexSettingsDoc,
    /// Live grok request settings (UI-3 U12 — group-settings bar effort
    /// override). Additive: absent in older docs → unavailable.
    #[serde(default)]
    pub grok: GrokSettingsDoc,
    /// Tokens-per-day chart rows (UI-3 U14), oldest first. Additive.
    #[serde(default)]
    pub daily_usage: Vec<DailyUsageDoc>,
    /// Observed-performance rows (perf telemetry v1), oldest day first.
    /// Additive: absent from an older daemon → empty Perf tab with a hint;
    /// `skip_serializing_if` keeps it off the wire with no history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub daily_perf: Vec<DailyPerfDoc>,
    /// Config-editor facts (additive: defaults on older daemons).
    #[serde(default)]
    pub config_facts: ConfigFactsDoc,
    /// Usage-tab calendar rows (usage-stats): every granularity flattened,
    /// newest bucket first, cost priced server-side (T6 — the attach client
    /// has no pricing overrides). Additive: absent from an older daemon →
    /// empty tab with a hint; `skip_serializing_if` keeps it off the wire
    /// when there is no history yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usage_stats: Vec<UsageStatDoc>,
    /// Live `email_anonymous` display setting. Account names in THIS document
    /// stay real (SSOT T1); the renderer masks at draw time when this is on,
    /// so an API flip reflects on the next frame/poll without restart — in
    /// BOTH TUI backends (local builds the doc in-process; attach receives it
    /// here). Additive: absent in docs from an older daemon → false.
    #[serde(default)]
    pub email_anonymous: bool,
    /// Config `tui_effects`: whether the TUI plays the cosmetic effort-token
    /// rainbow marquee and headline-model name gradient. Carried here so BOTH
    /// TUI backends honor the config setting (local builds the doc in-process;
    /// attach receives it here), same convention as `email_anonymous` /
    /// `show_fable_weekly`. Additive: absent in docs from an older daemon →
    /// the client defaults effects ON.
    #[serde(default = "default_true")]
    pub tui_effects: bool,
    /// Config `tui_gradient` (UI-8): gradient drift speed + base colors,
    /// carried like `tui_effects` so both TUI backends honor it. Additive:
    /// absent in docs from an older daemon → the built-in defaults.
    #[serde(default)]
    pub tui_gradient: crate::config::TuiGradient,
    /// Whether the TUI should render the model-scoped "Fable" weekly gauge in
    /// the accounts table (fable-usage U9a — config `show_fable_weekly`,
    /// default ON). The scoped data itself is ALWAYS emitted (`fable_weekly` /
    /// `scoped_limits` on each account below); this flag only gates the render,
    /// carried here so BOTH TUI backends honor the config setting (local builds
    /// the doc in-process; attach receives it here). Additive: absent in docs
    /// from an older daemon → the client defaults the gauge ON.
    #[serde(default = "default_true")]
    pub show_fable_weekly: bool,
    /// Domain abbreviations for the accounts table's name column (config
    /// `domain_abbrev`): `ai3@insightquest.io` renders `ai3@iq.io`. Carried
    /// here so BOTH TUI backends abbreviate identically (local builds the doc
    /// in-process; attach receives it here); account names in the document
    /// itself stay real, same convention as `email_anonymous`. Additive:
    /// absent in docs from an older daemon → the built-in default map.
    #[serde(default = "crate::config::default_domain_abbrev")]
    pub domain_abbrev: BTreeMap<String, String>,
    /// Fill direction for the TUI quota gauges (config `quota_display`):
    /// `remaining` (default) drains toward the reset, `used` fills as quota
    /// burns. Display-only boot default — the TUI `u` key overrides it live
    /// for the session. Additive: absent in docs from an older daemon →
    /// `remaining`.
    #[serde(default)]
    pub quota_display: crate::config::QuotaDisplay,
    /// Data-quality qualifiers for the derived statistics (issue #62 S2).
    /// The server is the SSOT for the label wording — the TUI and the Islands
    /// app render these strings verbatim instead of hardcoding copies that
    /// could drift. Additive: absent in docs from an older daemon → the serde
    /// default, which is byte-identical to the canonical wording by
    /// construction (see [`DataQualityDoc`]).
    #[serde(default)]
    pub data_quality: DataQualityDoc,
    /// Live top-of-dashboard event banners (config `events`), read from the
    /// daemon's live [`crate::proxy::server::AppState::event_banners`] holder
    /// each frame / poll. Carried here so BOTH TUI backends render them
    /// identically (local builds the doc in-process; attach receives it here)
    /// and a `POST /llmux/events` reflects on the next document without restart.
    /// Additive: `skip_serializing_if = "Vec::is_empty"` keeps it off the wire
    /// when empty, and an absent field parses back to an empty list, so an older
    /// client attaching to a newer daemon (and vice versa) is unaffected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<crate::config::EventBanner>,
}

/// Serde default for additive `bool` fields that default ON.
fn default_true() -> bool {
    true
}

/// Data-quality label wording for the dashboard's derived statistics
/// (issue #62 S2, gist-01 §Phase 4). One string per qualified surface:
///
/// - `model_usage` — scope of the per-model rows (persisted history hydrated
///   at startup + the live runtime's activity, not an upstream ledger),
/// - `windowed` — accuracy of the 24h/72h heatmap (lossy channel sample),
/// - `cost` — nature of every `$` figure (API-equivalent estimate, not a bill),
/// - `cache` — cache-counter semantics (absent upstream fields render as
///   unavailable, never as zero).
///
/// The SERVER owns this wording; the TUI and the Islands app render the
/// strings verbatim. `Default` returns exactly the canonical strings, and
/// each field also carries a per-field serde default, so a document from an
/// older daemon (field absent) — or a partial object from a skewed one —
/// parses into byte-identical labels. That IS the old-daemon fallback: no
/// client-side fallback constant can drift from the server wording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataQualityDoc {
    #[serde(default = "dq_model_usage")]
    pub model_usage: String,
    #[serde(default = "dq_windowed")]
    pub windowed: String,
    #[serde(default = "dq_cost")]
    pub cost: String,
    #[serde(default = "dq_cache")]
    pub cache: String,
}

fn dq_model_usage() -> String {
    "hydrated activity/runtime".to_string()
}

fn dq_windowed() -> String {
    "best effort".to_string()
}

fn dq_cost() -> String {
    "API-equivalent estimate".to_string()
}

fn dq_cache() -> String {
    "missing fields shown as unavailable".to_string()
}

impl Default for DataQualityDoc {
    fn default() -> Self {
        Self {
            model_usage: dq_model_usage(),
            windowed: dq_windowed(),
            cost: dq_cost(),
            cache: dq_cache(),
        }
    }
}

/// Live codex provider settings, surfaced so the dashboard can show and toggle
/// them (req8.1). `available` is false when no codex account is configured —
/// the dashboard then hides the controls.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodexSettingsDoc {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub fast: bool,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// One (day, group, model) row of the Tokens-per-Day chart (UI-3 U14).
/// `day` is epoch DAYS (UTC). Additive: absent in older docs → no chart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyUsageDoc {
    pub day: u64,
    pub group: String,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_creation: u64,
}

/// Config facts the TUI config editor renders that ride no other doc field
/// (config-editor v1, additive). Live-holder-backed values report the
/// EFFECTIVE state; restart-only values report the boot snapshot — after an
/// edit the daemon keeps running on the old value, and the editor labels the
/// row "restart required" instead of pretending it applied.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigFactsDoc {
    #[serde(default)]
    pub routing_enabled: bool,
    #[serde(default)]
    pub routing_default_group: String,
    #[serde(default)]
    pub routing_on_empty_group: String,
    #[serde(default)]
    pub raw_io_enabled: bool,
    #[serde(default)]
    pub raw_io_retention_days: u64,
    #[serde(default)]
    pub raw_io_max_body_bytes: u64,
    #[serde(default)]
    pub gradient_speed: f32,
    #[serde(default)]
    pub codex_upstream: String,
    #[serde(default)]
    pub proxy_max_request_bytes: u64,
    /// On-disk sizes of the persistence files (bytes), refreshed at most
    /// every 30s (never per frame — the misc tab reads these). `None` = file
    /// absent or stat failed.
    #[serde(default)]
    pub raw_io_bytes: Option<u64>,
    #[serde(default)]
    pub activity_log_bytes: Option<u64>,
}

/// 30s-TTL cache for the persistence-file sizes shown on the misc tab — a
/// stat per render frame would be wasteful; staleness up to the TTL is fine
/// for a glance fact.
fn persist_file_sizes(
    raw_io: Option<&std::path::Path>,
    activity: Option<&std::path::Path>,
) -> (Option<u64>, Option<u64>) {
    use std::sync::Mutex;
    use std::time::Instant;
    type SizesAt = (Instant, Option<u64>, Option<u64>);
    static CACHE: Mutex<Option<SizesAt>> = Mutex::new(None);
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((at, raw, act)) = *cache {
        if at.elapsed() < std::time::Duration::from_secs(30) {
            return (raw, act);
        }
    }
    let stat =
        |p: Option<&std::path::Path>| p.and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len());
    let fresh = (stat(raw_io), stat(activity));
    *cache = Some((Instant::now(), fresh.0, fresh.1));
    fresh
}

/// One (day, group, model, fast) row of the observed-performance stats
/// (perf telemetry v1 — the Perf tab). `day` is epoch DAYS (UTC, same
/// bucketing as [`DailyUsageDoc`]). All counters are raw sums; clients derive
/// throughput as `Σoutput/Σms` and NEVER average per-request rates. `fast`
/// is three-state: `None` = recorded before the field existed ("unknown"),
/// rendered as its own series — never merged into fast=off. Additive: absent
/// in older docs → no perf rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyPerfDoc {
    pub day: u64,
    pub group: String,
    pub model: String,
    #[serde(default)]
    pub fast: Option<bool>,
    pub requests: u64,
    pub ok: u64,
    pub errors: u64,
    /// Throughput samples (output > 0, duration > 0) and their raw sums.
    pub tps_n: u64,
    pub output_tokens: u64,
    pub e2e_ms: u64,
    /// Measured subset (ttft present, duration > ttft) and its raw sums —
    /// the only samples in the "estimated post-delta" series.
    #[serde(default)]
    pub measured_n: u64,
    #[serde(default)]
    pub measured_output: u64,
    #[serde(default)]
    pub post_ttft_ms: u64,
    /// TTFB observations.
    #[serde(default)]
    pub ttfb_n: u64,
    #[serde(default)]
    pub ttfb_ms_sum: u64,
}

/// One (bucket, group, model) row of the Usage tab (usage-stats).
///
/// `gran` is the stable wire tag (`"hour"` / `"day"` / `"month"` —
/// [`crate::tui::activity::UsageGran::tag`]); `bucket` is the granularity's
/// sort key (epoch hours / local civil days / `year*12+month0`); `label` is
/// the server-rendered calendar label (daemon-local civil dates — the daemon
/// owns what "a day" means, so local and attach clients can never disagree).
/// `cost_usd` is the API-equivalent estimate priced at doc-build time with
/// the daemon's pricing overrides (see [`DataQualityDoc::cost`]). Additive:
/// absent in older docs → no usage rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageStatDoc {
    pub gran: String,
    pub bucket: u64,
    pub label: String,
    pub group: String,
    pub model: String,
    pub requests: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_creation: u64,
    #[serde(default)]
    pub cost_usd: f64,
    /// Whether a price was found for this row's `(group, model)` (config
    /// override, built-in table, or group fallback). `false` means the `0.0`
    /// in `cost_usd` is "no rate known", NOT "free" — the renderer shows `—`
    /// instead of a fabricated `$0` (review R1 MUST-FIX 3).
    #[serde(default = "default_true")]
    pub priced: bool,
}

/// Live grok provider settings (UI-3 U12), mirroring [`CodexSettingsDoc`]:
/// `available` is false when no grok account is configured; `effort` `None`
/// = bypass (the client's `output_config.effort` rides through).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GrokSettingsDoc {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SelectParamsDoc {
    pub five_hour_max: f64,
    pub seven_day_max: f64,
    /// Fable-weekly ceiling (additive: docs from older daemons default 0.98).
    #[serde(default = "crate::config::default_fable_weekly_max")]
    pub fable_weekly_max: f64,
    /// Selection algorithm (additive: docs from older daemons default
    /// `default`). Carried so attach-mode display order / next-in-line match
    /// the daemon's actual scheduler.
    #[serde(default)]
    pub mode: crate::config::SchedulerMode,
    pub usage_max_age_secs: u64,
}

impl From<&SelectParams> for SelectParamsDoc {
    fn from(params: &SelectParams) -> Self {
        Self {
            five_hour_max: params.five_hour_max,
            seven_day_max: params.seven_day_max,
            fable_weekly_max: params.fable_weekly_max,
            mode: params.mode,
            usage_max_age_secs: params.usage_max_age.as_secs(),
        }
    }
}

impl From<&SelectParamsDoc> for SelectParams {
    fn from(doc: &SelectParamsDoc) -> Self {
        Self {
            five_hour_max: doc.five_hour_max,
            seven_day_max: doc.seven_day_max,
            fable_weekly_max: doc.fable_weekly_max,
            mode: doc.mode,
            usage_max_age: Duration::from_secs(doc.usage_max_age_secs),
        }
    }
}

/// One account, status-document-compatible plus the raw scheduler fields the
/// remote view-model needs to re-run the pure eligibility/ranking functions
/// client-side (`healthy`, window `fetched_at_ms`/`source`,
/// `cooldown_source`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountDoc {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    pub order: u64,
    pub blocked: Option<String>,
    pub healthy: bool,
    pub five_hour: Option<WindowDoc>,
    pub seven_day: Option<WindowDoc>,
    /// The "Fable" model-scoped weekly window surfaced for convenient reads;
    /// `null` when this account carries no Fable scope. Additive — absent in
    /// docs written before scoped windows existed.
    #[serde(default)]
    pub fable_weekly: Option<ScopedWindowDoc>,
    /// The full generic list of model-scoped weekly windows (`fable_weekly` is
    /// just the "Fable" entry surfaced above). Empty when none seen. Additive.
    #[serde(default)]
    pub scoped_limits: Vec<ScopedLimitDoc>,
    /// Epoch seconds (status parity); only present while cooling.
    pub cooldown_until: Option<u64>,
    pub cooldown_source: Option<String>,
    pub in_flight: u32,
    pub token_expires_at_ms: Option<u64>,
    pub last_refresh_ms: Option<u64>,
    /// Operator pause (config `paused_accounts`): the scheduler will not
    /// auto-select this account, and manual switch is refused until resumed.
    /// Additive: absent in docs from an older daemon → false.
    #[serde(default)]
    pub paused: bool,
    /// Per-account ceiling overrides (config `account_limits`); absent/null =
    /// the global scheduler ceilings apply. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<crate::config::AccountLimits>,
    /// Proxy-lifetime relayed totals (status parity).
    pub totals: LifetimeTotalsDoc,
    /// Activity-log totals (ok/err + token split) for the table/detail panes.
    pub session: SessionTotalsDoc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowDoc {
    pub utilization: f64,
    /// Epoch seconds (status parity).
    pub resets_at: u64,
    pub resets_in_secs: u64,
    /// Epoch ms — staleness is judged against this client-side.
    pub fetched_at_ms: u64,
    /// "headers" | "poll".
    pub source: String,
}

/// One model-scoped weekly window (`limits[]` `weekly_scoped`, e.g. the
/// "Fable" gauge) in document form. Shares the `utilization`/`resets_at`/
/// `resets_in_secs` shape with [`WindowDoc`] (built by the same helper) and
/// adds the scope row's `severity` + `is_active`. Deliberately omits
/// `fetched_at_ms`/`source`: the scoped list is a convenience read-surface,
/// not a scheduler-reconstruction input (the reconstructable account windows
/// stay [`WindowDoc`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedWindowDoc {
    pub utilization: f64,
    /// Epoch seconds (status parity).
    pub resets_at: u64,
    pub resets_in_secs: u64,
    /// Lowercase upstream severity label ("normal" | "warning" | "critical").
    pub severity: String,
    pub is_active: bool,
    /// Reset-aware "this limit is actually constraining right now" bool, from
    /// [`ScopedQuotaWindow::is_constraining`]. Unlike `severity`, this
    /// short-circuits on an expired/just-reset window, so a client (e.g. the
    /// Swift islands app, which can't call `is_constraining`) can paint the red
    /// state without re-flashing red on a post-reset 0% window whose `severity`
    /// field is still a stale `Critical`. `#[serde(default)]` (→ `false`) so a
    /// newer client parsing an older daemon's doc that predates the field
    /// degrades to "not constraining" instead of failing the whole parse —
    /// mirrors the wire-compat convention of `show_fable_weekly` /
    /// `email_anonymous` and the Swift decode's optional `constraining`.
    #[serde(default)]
    pub constraining: bool,
}

/// One entry of the generic `scoped_limits` list: a [`ScopedWindowDoc`] tagged
/// with its scope label (`scope.model.display_name`). Flattened so the entry
/// serializes as a flat `{ scope_label, utilization, resets_at,
/// resets_in_secs, severity, is_active }` object — future scoped models appear
/// here without another schema change. The flattened object gained a
/// `constraining` bool alongside `severity`/`is_active`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedLimitDoc {
    pub scope_label: String,
    #[serde(flatten)]
    pub window: ScopedWindowDoc,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct LifetimeTotalsDoc {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct SessionTotalsDoc {
    pub requests: u64,
    pub ok: u64,
    pub errors: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerDoc {
    pub last_switch: Option<LastSwitchDoc>,
    /// First eligible non-current account in selection order — what `pick`
    /// would switch to next.
    pub next_in_line: Option<String>,
    pub next_eval_in_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastSwitchDoc {
    pub from: Option<String>,
    pub to: String,
    pub reason: Option<String>,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollerDoc {
    pub account: String,
    pub last_ok_ms: Option<u64>,
    pub consecutive_failures: u32,
    pub next_at_ms: u64,
}

/// Rolling 5-minute status-class counts for the header health verdict
/// (glance-triage). Mirrors [`crate::tui::activity::HealthCounts`] 1:1 so
/// local and attach render the identical verdict.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct HealthDoc {
    pub requests: u64,
    pub errors: u64,
    pub s429: u64,
    pub s401: u64,
    pub s5xx: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct GlobalTotalsDoc {
    pub requests: u64,
    pub ok: u64,
    pub errors: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub rpm_5m: f64,
    pub in_flight: u32,
    /// API-equivalent USD cost (Feature D), defined as the **sum of the
    /// per-model-row costs** ([`ModelUsageDoc::cost_usd`]). Summing per row is
    /// the only correct aggregation because the global `tokens_in`/`tokens_out`
    /// mix models with different rates; a row already knows its own model's
    /// price. Additive: absent in docs written before Feature D → `0.0`.
    #[serde(default)]
    pub cost_usd: f64,
}

/// One model-usage row in the document (req1-20). Cache counters are omitted
/// from the JSON when unavailable (`None`), so the client distinguishes
/// "unavailable" from an explicit zero.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsageDoc {
    pub group: String,
    pub model: String,
    pub requests: u64,
    pub ok: u64,
    pub errors: u64,
    /// Fresh (non-cached) input + output tokens.
    pub tokens_in: u64,
    pub tokens_out: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<u64>,
    /// Epoch ms of the last completed request for this model.
    pub last_used_ms: u64,
    /// In-flight requests currently attributed to this model (req11).
    #[serde(default)]
    pub in_flight: u32,
    /// Which account(s) served it (req19).
    #[serde(default)]
    pub accounts: Vec<ModelAccountDoc>,
    /// Reasoning/effort distribution (req18).
    #[serde(default)]
    pub efforts: Vec<ModelCountDoc>,
    /// Endpoint-class distribution (req20).
    #[serde(default)]
    pub endpoints: Vec<ModelCountDoc>,
    /// API-equivalent USD cost estimate for this model row (issue #62 S1):
    /// the row's accumulated token parts priced via [`crate::pricing`] with
    /// the daemon's pricing overrides, so clients need not duplicate the rate
    /// table. Additive: absent in docs from older daemons → `0.0` (a client
    /// may recompute from the row's tokens as a fallback).
    #[serde(default)]
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAccountDoc {
    pub name: String,
    pub requests: u64,
    pub ok: u64,
    pub errors: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

/// A labelled request count (an effort level or an endpoint class).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCountDoc {
    pub label: String,
    pub requests: u64,
}

/// One per-client attribution row (issue #32): a client identity
/// (`metadata.user_id`, or `unknown`) and its in-memory request/token counts.
/// Counting only — never a credential, never gates a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientUsageDoc {
    pub client: String,
    pub requests: u64,
    pub ok: u64,
    pub errors: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// API-equivalent USD cost attributed to this client (issue #62 S1).
    /// Wire-ready additive field: the hub's per-client tracking keeps only
    /// request/token totals today (no per-model attribution to price
    /// against), so the daemon emits `0.0` until that tracking exists.
    #[serde(default)]
    pub cost_usd: f64,
    /// Epoch ms of this client's most recent request (issue #62 S1).
    /// Wire-ready additive field: the hub does not track per-client
    /// last-seen today, so the daemon emits `0` until it does.
    #[serde(default)]
    pub last_seen_ms: u64,
}

/// One per-tenant attribution row (multi-tenant #22): the stable tenant id,
/// its display name resolved at build time (key name; the id itself for the
/// builtin `local`/`legacy`/`unknown` buckets), and its lifetime counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantUsageDoc {
    /// Stable attribution id: `k-…` / `legacy` / `local` / `unknown`.
    pub tenant: String,
    /// Display name (key name, or the bucket id itself).
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
    pub requests: u64,
    pub ok: u64,
    pub errors: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// API-equivalent USD cost, summed over the priced per-model cells.
    #[serde(default)]
    pub cost_usd: f64,
    /// First/last finished-request stamps (epoch ms; 0 = no data) — the
    /// "used from … to …" span the keys panel renders.
    #[serde(default)]
    pub first_ms: u64,
    #[serde(default)]
    pub last_ms: u64,
    /// Per-(group, model) breakdown, sorted by total tokens desc.
    #[serde(default)]
    pub models: Vec<TenantModelDoc>,
}

/// One tenant's usage of one served model (multi-tenant #22).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantModelDoc {
    pub group: String,
    pub model: String,
    pub requests: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    /// API-equivalent USD cost for this cell (0.0 when nothing prices it).
    pub cost_usd: f64,
}

/// One issued client key's metadata for the dashboard (multi-tenant #22).
/// NEVER carries the secret or digest — display fields only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRowDoc {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
    /// `"default"` or `"admin"`.
    pub kind: String,
    pub key_prefix: String,
    pub suspended: bool,
    pub created_at_ms: u64,
    #[serde(default)]
    pub revoked_at_ms: Option<u64>,
}

/// One trailing-window slice of the per-account/per-model heatmap (issue #23):
/// a window label ("24h"/"72h") + every `(group, model, account)` cell with
/// activity in that window, sorted by total tokens desc. Additive document
/// type, carried in [`DashboardDoc::windowed`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowedStatsDoc {
    /// Short window label ("24h" / "72h").
    pub window: String,
    /// Trailing duration this window covers, in seconds (so a client need not
    /// hardcode the label→duration map).
    pub window_secs: u64,
    pub cells: Vec<WindowedCellDoc>,
}

/// One `(group, model, account)` heatmap cell over a window. Token fields are
/// the in-window sums; `tokens` is the combined intensity (in+out+cache) the
/// heatmap colours by.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowedCellDoc {
    pub group: String,
    pub model: String,
    pub account: String,
    pub requests: u64,
    pub ok: u64,
    pub errors: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_creation: u64,
    /// Combined token intensity (in + out + cache_read + cache_creation).
    pub tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityDoc {
    /// Started-but-unfinished requests, oldest→newest (render reversed).
    pub in_flight: Vec<InFlightDoc>,
    /// Completed entries, newest first, capped at [`ACTIVITY_TAIL`].
    pub completed: Vec<CompletedDoc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InFlightDoc {
    pub id: u64,
    pub method: String,
    pub path: String,
    pub account: Option<String>,
    pub started_at_ms: u64,
    /// Backend group / served model / per-request effort / fast, filled at
    /// routing time so the in-flight row can show the same metadata badge as a
    /// completed row while running (issue #2 2a). Additive: absent in docs
    /// written before these fields existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
    /// Message-kind classification, known at start time (TUI UI-6 item 1) so
    /// the attached in-flight row shows the same `kind` column as a completed
    /// row. Additive: absent in docs written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

// Request dwarfs Note by design (see `tui::activity::CompletedBody`): almost
// every entry is a Request, so the boxed-variant fix would cost an allocation
// per real entry.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletedDoc {
    Request {
        /// Activity id — the raw-io correlation key for the raw viewer
        /// (paired with `at_ms` to disambiguate across daemon restarts).
        /// Additive: absent (→ `0` = unknown) in docs from older daemons.
        #[serde(default)]
        id: u64,
        at_ms: u64,
        method: String,
        path: String,
        account: Option<String>,
        status: u16,
        duration_ms: u64,
        tokens: Option<TokensDoc>,
        /// API-equivalent USD cost (Feature D) for this single request, from
        /// its `(group, model)` + `tokens` via [`crate::pricing::cost_usd`].
        /// `0.0` when group/model/tokens are not all known, or the model is
        /// unknown/zero-rate. Additive: absent in pre-Feature-D docs → `0.0`.
        #[serde(default)]
        cost_usd: f64,
        /// Backend group / served model / reasoning effort (additive: absent
        /// in docs written before these fields existed).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        /// Codex fast mode was in effect (`Some(false)` for claude; `None` =
        /// pre-field replayed history — "unknown"). Additive both ways:
        /// absent (→ `None`) in docs written before this field existed, and
        /// `None` is skipped on the wire so an OLDER attach client's plain
        /// `bool` default still parses.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fast: Option<bool>,
        /// Perf telemetry v1 (additive): millis to first upstream body chunk
        /// and to the first streamed output delta.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttfb_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttft_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gen_ms: Option<u64>,
        /// Upstream stream aborted mid-body. Skipped when false so older
        /// attach clients keep parsing.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        aborted: bool,
        /// Client identity / message kind / input excerpt (TUI UI-3 U1/U2).
        /// Additive: absent in docs written before these fields existed.
        ///
        /// PRIVACY: `excerpt` carries up to 400 chars of PROMPT TEXT into
        /// this document and the persisted request log — the same class of
        /// content the raw-io capture already stores verbatim, behind the
        /// same boundary (loopback-only bind or `proxy.api_key`). The
        /// renderer-side `email_anonymous` masking applies at DRAW time
        /// only; it does not redact this wire field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        msg_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        excerpt: Option<String>,
        /// KEYED tenant attribution id (`k-…` / `legacy` / `local`) and its
        /// resolved display name (key name for `k-…`, the bucket id itself
        /// for builtins — same join as `tenant_usage`). Additive: absent in
        /// docs written before these fields existed, and `None` (pre-tenant
        /// replayed history) is skipped on the wire — never coerced.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tenant: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_name: Option<String>,
    },
    Note {
        at_ms: u64,
        text: String,
        error: bool,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TokensDoc {
    pub input: u64,
    pub output: u64,
    /// Cache-read / cache-write splits, when the upstream reported them.
    /// `None` (absent on the wire) is distinct from `Some(0)` — the TUI detail
    /// row renders unavailable as `—`. Additive: docs written before these
    /// fields existed deserialize as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLineDoc {
    /// "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE".
    pub level: String,
    pub text: String,
}

/// Server-process facts + config-derived display fields for one document.
#[derive(Debug, Clone)]
pub struct DocMeta {
    pub pid: u32,
    pub uptime_secs: u64,
    pub port: u16,
    pub upstream: String,
    pub config_path: Option<String>,
    pub refresh_ahead_secs: u64,
    pub evaluate_tick_secs: u64,
    pub codex: CodexSettingsDoc,
    /// Live grok settings (UI-3 U12), same convention as `codex`.
    pub grok: GrokSettingsDoc,
    /// Live `email_anonymous` display setting (see
    /// [`DashboardDoc::email_anonymous`]).
    pub email_anonymous: bool,
    /// Config `tui_effects`: whether the TUI plays cosmetic animations. See
    /// [`DashboardDoc::tui_effects`].
    pub tui_effects: bool,
    /// Config `tui_gradient` (UI-8). See [`DashboardDoc::tui_gradient`].
    pub tui_gradient: crate::config::TuiGradient,
    /// Config `show_fable_weekly` (fable-usage U9a): whether the TUI renders
    /// the Fable weekly gauge. See [`DashboardDoc::show_fable_weekly`].
    pub show_fable_weekly: bool,
    /// Config `domain_abbrev`: accounts-table domain abbreviations. See
    /// [`DashboardDoc::domain_abbrev`].
    pub domain_abbrev: BTreeMap<String, String>,
    /// Config `quota_display`: quota-gauge fill direction. See
    /// [`DashboardDoc::quota_display`].
    pub quota_display: crate::config::QuotaDisplay,
    /// API-equivalent pricing overrides from `[pricing]` in the live config
    /// (Feature D). Empty = use the built-in default rate table. Threaded here
    /// (rather than into the pure `dashboard_doc` signature) because `DocMeta`
    /// already carries the config-derived display fields the builder needs.
    pub pricing_overrides: HashMap<String, crate::pricing::ModelPrice>,
    /// Live event banners (config `events`), read from the daemon's live
    /// [`crate::proxy::server::AppState::event_banners`] holder. See
    /// [`DashboardDoc::events`].
    pub events: Vec<crate::config::EventBanner>,
    /// Config-editor facts (see [`ConfigFactsDoc`]).
    pub config_facts: ConfigFactsDoc,
    /// Issued client keys (metadata only), from the LIVE registry.
    pub client_keys: Vec<KeyRowDoc>,
}

pub(crate) fn epoch_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn epoch_secs(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Derive the status word + blocking reason for one account — shared by
/// `/llmux/status` and `/llmux/dashboard` so the wording never
/// drifts between the two documents.
pub(crate) fn account_status_blocked(
    account: &AccountSnapshot,
    snapshot: &PoolSnapshot,
    params: &SelectParams,
    now: SystemTime,
    headers_only: bool,
) -> (&'static str, Option<String>) {
    let cooling = account.cooldown_until.is_some_and(|until| until > now);
    let status = if !account.healthy {
        "auth_failed"
    } else if cooling {
        "cooldown"
    } else if snapshot.is_current(&account.id) {
        "active"
    } else {
        "ok"
    };
    let blocked = select::eligibility(account, params, now, headers_only)
        .map(|reason| select::blocking_reason(account, reason, params, now));
    (status, blocked)
}

fn window_doc(
    window: &Option<crate::scheduler::window::QuotaWindow>,
    now: SystemTime,
) -> Option<WindowDoc> {
    window.map(|w| WindowDoc {
        utilization: w.utilization,
        resets_at: epoch_secs(w.resets_at),
        resets_in_secs: w
            .resets_at
            .duration_since(now)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        fetched_at_ms: epoch_ms(w.fetched_at),
        source: match w.source {
            crate::scheduler::window::WindowSource::Headers => "headers".into(),
            crate::scheduler::window::WindowSource::UsagePoll => "poll".into(),
        },
    })
}

/// Document form of one model-scoped window. Utilization is the RAW window
/// value (same convention as [`window_doc`], which keeps the raw number so the
/// client can compute its own expiry from the reconstruction fields); the
/// account-window docs and this one stay consistent within the dashboard doc.
fn scoped_window_doc(
    scoped: &crate::scheduler::window::ScopedQuotaWindow,
    now: SystemTime,
    threshold: f64,
) -> ScopedWindowDoc {
    ScopedWindowDoc {
        utilization: scoped.window.utilization,
        resets_at: epoch_secs(scoped.window.resets_at),
        resets_in_secs: scoped
            .window
            .resets_at
            .duration_since(now)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        severity: scoped.severity.label().to_string(),
        is_active: scoped.is_active,
        constraining: scoped.is_constraining(now, threshold),
    }
}

/// Build the model-usage rows for the document: the finished aggregation from
/// the hub, with in-flight requests overlaid per model (req11). A model seen
/// only in-flight (no completed request yet) still gets a row so a long active
/// request is visible before it finishes.
///
/// Each completed row carries its own API-equivalent USD cost
/// ([`ModelUsageDoc::cost_usd`], issue #62 S1), computed once here from the
/// row's token parts + the daemon's pricing overrides. The returned total
/// (Feature D) is the **sum of those per-row costs** — the only correct
/// aggregation, since each model carries its own rate. In-flight-only rows
/// have no completed tokens and contribute `0`.
fn model_usage_docs(
    hub: &HubView,
    now: SystemTime,
    pricing_overrides: &HashMap<String, crate::pricing::ModelPrice>,
) -> (Vec<ModelUsageDoc>, f64) {
    let row = |m: &ModelUsage| ModelUsageDoc {
        group: m.group.clone(),
        model: m.model.clone(),
        requests: m.requests,
        ok: m.ok,
        errors: m.errors,
        tokens_in: m.tokens_in,
        tokens_out: m.tokens_out,
        cache_read: m.cache_read,
        cache_creation: m.cache_creation,
        last_used_ms: epoch_ms(m.last_used),
        in_flight: 0,
        cost_usd: crate::pricing::cost_from_parts(
            &m.group,
            &m.model,
            m.tokens_in,
            m.tokens_out,
            m.cache_read,
            m.cache_creation,
            pricing_overrides,
        ),
        accounts: m
            .accounts
            .iter()
            .map(|a| ModelAccountDoc {
                name: a.name.clone(),
                requests: a.requests,
                ok: a.ok,
                errors: a.errors,
                tokens_in: a.tokens_in,
                tokens_out: a.tokens_out,
            })
            .collect(),
        efforts: m
            .efforts
            .iter()
            .map(|c| ModelCountDoc {
                label: c.label.clone(),
                requests: c.requests,
            })
            .collect(),
        endpoints: m
            .endpoints
            .iter()
            .map(|c| ModelCountDoc {
                label: c.label.clone(),
                requests: c.requests,
            })
            .collect(),
    };
    let mut docs: Vec<ModelUsageDoc> = hub.model_usage.iter().map(row).collect();

    // Count in-flight requests per (group, normalized model) — the in-flight
    // entries carry the served identity set at routing time.
    let mut in_flight: BTreeMap<(String, String), u32> = BTreeMap::new();
    for r in &hub.in_flight {
        if let (Some(group), Some(model)) = (&r.group, &r.model) {
            *in_flight
                .entry((group.clone(), normalize_model(model)))
                .or_default() += 1;
        }
    }
    for doc in docs.iter_mut() {
        if let Some(n) = in_flight.remove(&(doc.group.clone(), doc.model.clone())) {
            doc.in_flight = n;
        }
    }
    // Append rows for models that are ONLY in-flight (sorted by the BTreeMap).
    for ((group, model), n) in in_flight {
        docs.push(ModelUsageDoc {
            group,
            model,
            requests: 0,
            ok: 0,
            errors: 0,
            tokens_in: 0,
            tokens_out: 0,
            cache_read: None,
            cache_creation: None,
            last_used_ms: epoch_ms(now),
            in_flight: n,
            accounts: Vec::new(),
            efforts: Vec::new(),
            endpoints: Vec::new(),
            // No completed request yet → no accumulated tokens to price.
            // The per-request cost lands on the row once the request finishes.
            cost_usd: 0.0,
        });
    }
    // Global total = Σ per-row costs, by construction (in-flight-only rows
    // are 0.0) — the invariant the doc tests pin down.
    let total_cost: f64 = docs.iter().map(|d| d.cost_usd).sum();
    (docs, total_cost)
}

/// Build the windowed heatmap document rows from the hub view (issue #23): one
/// [`WindowedStatsDoc`] per retained window, each carrying every in-window
/// `(group, model, account)` cell. The rows are already sorted by the hub.
fn windowed_docs(hub: &HubView) -> Vec<WindowedStatsDoc> {
    hub.windowed
        .iter()
        .map(|(window, rows)| WindowedStatsDoc {
            window: window.label().to_string(),
            window_secs: window.duration().as_secs(),
            cells: rows
                .iter()
                .map(|r: &WindowedRow| WindowedCellDoc {
                    group: r.group.clone(),
                    model: r.model.clone(),
                    account: r.account.clone(),
                    requests: r.counts.requests,
                    ok: r.counts.ok,
                    errors: r.counts.errors,
                    tokens_in: r.counts.tokens_in,
                    tokens_out: r.counts.tokens_out,
                    cache_read: r.counts.cache_read,
                    cache_creation: r.counts.cache_creation,
                    tokens: r.counts.tokens(),
                })
                .collect(),
        })
        .collect()
}

/// Build the dashboard document — pure over snapshot/hub/totals/params so
/// the shape is unit-testable without a socket.
pub(crate) fn dashboard_doc(
    snapshot: &PoolSnapshot,
    hub: &HubView,
    totals: &UsageTotals,
    params: &SelectParams,
    now: SystemTime,
    meta: &DocMeta,
) -> DashboardDoc {
    let headers_only = select::headers_only_mode(snapshot, params, None, now);
    let order = select::selection_order(snapshot, params, now);
    let accounts: Vec<AccountDoc> = order
        .iter()
        .enumerate()
        .map(|(pos, &idx)| {
            let account = &snapshot.accounts[idx];
            let (status, blocked) =
                account_status_blocked(account, snapshot, params, now, headers_only);
            let cooling = account.cooldown_until.is_some_and(|until| until > now);
            // The account's EFFECTIVE Fable ceiling: per-account override else
            // global — the doc's `constraining` then matches the selector.
            let fable_max = select::effective_limits(account, params).2;
            let lifetime = totals.get(&account.id);
            let session = hub
                .account_totals
                .get(&account.id.0)
                .copied()
                .unwrap_or_default();
            AccountDoc {
                name: account.id.0.clone(),
                kind: account.credential_kind.to_string(),
                status: status.to_string(),
                order: pos as u64 + 1,
                blocked,
                healthy: account.healthy,
                five_hour: window_doc(&account.five_hour, now),
                seven_day: window_doc(&account.seven_day, now),
                fable_weekly: account
                    .fable_weekly()
                    .map(|s| scoped_window_doc(s, now, fable_max)),
                scoped_limits: account
                    .scoped_limits
                    .iter()
                    .map(|s| ScopedLimitDoc {
                        scope_label: s.scope_label.clone(),
                        window: scoped_window_doc(s, now, fable_max),
                    })
                    .collect(),
                cooldown_until: account.cooldown_until.filter(|_| cooling).map(epoch_secs),
                cooldown_source: account.cooldown_source.map(|s| match s {
                    CooldownSource::RetryAfter => "retry_after".to_string(),
                    CooldownSource::Heuristic => "heuristic".to_string(),
                }),
                in_flight: account.in_flight,
                token_expires_at_ms: account.token_expires_at_ms,
                last_refresh_ms: account.last_refresh_ms,
                paused: account.paused,
                limits: (!account.limits.is_empty()).then_some(account.limits),
                totals: LifetimeTotalsDoc {
                    requests: lifetime.requests,
                    input_tokens: lifetime.input_tokens,
                    output_tokens: lifetime.output_tokens,
                },
                session: SessionTotalsDoc {
                    requests: session.requests,
                    ok: session.ok,
                    errors: session.errors,
                    tokens_in: session.tokens_in,
                    tokens_out: session.tokens_out,
                },
            }
        })
        .collect();

    // First eligible non-current account in selection order.
    let next_in_line = order
        .iter()
        .map(|&i| &snapshot.accounts[i])
        .filter(|a| !snapshot.is_current(&a.id))
        .find(|a| select::eligibility(a, params, now, headers_only).is_none())
        .map(|a| a.id.0.clone());
    let tick = meta.evaluate_tick_secs.max(1);
    let scheduler = SchedulerDoc {
        last_switch: hub.last_switch.as_ref().map(|s| LastSwitchDoc {
            from: s.from.clone(),
            to: s.to.clone(),
            reason: s.reason.clone(),
            at_ms: epoch_ms(s.at),
        }),
        next_in_line,
        next_eval_in_secs: tick - (meta.uptime_secs % tick),
    };

    let mut poller: Vec<PollerDoc> = hub
        .poll_health
        .iter()
        .map(|(account, health)| PollerDoc {
            account: account.clone(),
            last_ok_ms: health.last_ok.map(epoch_ms),
            consecutive_failures: health.consecutive_failures,
            next_at_ms: epoch_ms(health.next_at),
        })
        .collect();
    poller.sort_by(|a, b| a.account.cmp(&b.account));

    let in_flight_total: u32 = snapshot.accounts.iter().map(|a| a.in_flight).sum();
    let activity = ActivityDoc {
        in_flight: hub
            .in_flight
            .iter()
            .map(|r| InFlightDoc {
                id: r.id,
                method: r.method.clone(),
                path: r.path.clone(),
                account: r.account.clone(),
                started_at_ms: epoch_ms(r.started_at),
                group: r.group.clone(),
                model: r.model.clone(),
                effort: r.effort.clone(),
                fast: r.fast,
                kind: r.kind.clone(),
            })
            .collect(),
        completed: hub
            .completed
            .iter()
            .take(ACTIVITY_TAIL)
            .map(|entry| match &entry.body {
                CompletedBody::Request {
                    id,
                    method,
                    path,
                    account,
                    status,
                    duration,
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
                    kind,
                    excerpt,
                    tenant,
                    // Fold-time name is always None (no key metadata at the
                    // fold) — the doc resolves it below from `meta`.
                    client_name: _,
                } => CompletedDoc::Request {
                    id: *id,
                    at_ms: epoch_ms(entry.at),
                    method: method.clone(),
                    path: path.clone(),
                    account: account.clone(),
                    status: *status,
                    duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
                    tokens: tokens.map(|t| TokensDoc {
                        input: t.input,
                        output: t.output,
                        cache_read: t.cache_read,
                        cache_creation: t.cache_creation,
                    }),
                    // Per-request API-equivalent cost: 0.0 unless group, model,
                    // and the upstream token usage are ALL known (Feature D).
                    cost_usd: match (group, model, tokens) {
                        (Some(g), Some(m), Some(t)) => {
                            crate::pricing::cost_usd(g, m, t, &meta.pricing_overrides)
                        }
                        _ => 0.0,
                    },
                    group: group.clone(),
                    model: model.clone(),
                    effort: effort.clone(),
                    fast: *fast,
                    ttfb_ms: *ttfb_ms,
                    ttft_ms: *ttft_ms,
                    gen_ms: *gen_ms,
                    aborted: *aborted,
                    user_id: user_id.clone(),
                    msg_kind: kind.clone(),
                    excerpt: excerpt.clone(),
                    tenant: tenant.clone(),
                    // Per-row client name (activity Name column): key name
                    // for `k-…` ids, the id itself for builtin buckets
                    // (`local`/`legacy`) — the same join as `tenant_usage`
                    // below. `None` tenant (pre-field history) stays `None`,
                    // never coerced into `local`.
                    client_name: tenant.as_ref().map(|id| {
                        meta.client_keys
                            .iter()
                            .find(|k| &k.id == id)
                            .map(|k| k.name.clone())
                            .unwrap_or_else(|| id.clone())
                    }),
                },
                CompletedBody::Note { text, error } => CompletedDoc::Note {
                    at_ms: epoch_ms(entry.at),
                    text: text.clone(),
                    error: *error,
                },
            })
            .collect(),
    };

    // Build model rows once; the builder also returns the global cost as the
    // sum of each completed row's per-model cost (Feature D).
    let (model_usage, total_cost_usd) = model_usage_docs(hub, now, &meta.pricing_overrides);

    // Usage-tab calendar rows (usage-stats): price each row server-side (T6)
    // — the attach client has no pricing overrides to price with. ONE
    // `priced_cost` lookup yields both the cost and the `priced` flag, so a
    // rate-less row can never masquerade as free (review R1 MUST-FIX 3) and
    // the two fields cannot diverge.
    let usage_stats: Vec<UsageStatDoc> = hub
        .usage_stats
        .iter()
        .map(|r| {
            let cost = crate::pricing::priced_cost(
                &r.group,
                &r.model,
                r.tokens_in,
                r.tokens_out,
                Some(r.cache_read),
                Some(r.cache_creation),
                &meta.pricing_overrides,
            );
            UsageStatDoc {
                priced: cost.is_some(),
                cost_usd: cost.unwrap_or(0.0),
                ..r.clone()
            }
        })
        .collect();

    // Per-client attribution rows (issue #32): already sorted (requests desc)
    // by the activity log; a direct projection to the wire type.
    let client_usage: Vec<ClientUsageDoc> = hub
        .client_usage
        .iter()
        .map(|c| ClientUsageDoc {
            client: c.client.clone(),
            requests: c.requests,
            ok: c.ok,
            errors: c.errors,
            tokens_in: c.tokens_in,
            tokens_out: c.tokens_out,
            // Wire-ready (issue #62 S1): the hub tracks per-client totals
            // only — no per-model attribution to price, no last-seen stamp.
            // Emit the serde defaults until that tracking exists.
            cost_usd: 0.0,
            last_seen_ms: 0,
        })
        .collect();

    // Per-tenant rows (multi-tenant #22): join the hub's aggregates with the
    // key metadata (names/emails) and price each model cell — same rate path
    // as the model-usage rows, overrides included. Builtin buckets
    // (`local`/`legacy`/`unknown`) name themselves.
    let mut tenant_usage: Vec<TenantUsageDoc> = hub
        .tenant_stats
        .iter()
        .map(|(id, t)| {
            let key = meta.client_keys.iter().find(|k| &k.id == id);
            let mut models: Vec<TenantModelDoc> = t
                .models
                .iter()
                .map(|((group, model), cell)| {
                    let tokens = TokenCounts {
                        input: cell.input,
                        output: cell.output,
                        cache_read: Some(cell.cache_read),
                        cache_creation: Some(cell.cache_creation),
                    };
                    TenantModelDoc {
                        group: group.clone(),
                        model: model.clone(),
                        requests: cell.requests,
                        tokens_in: cell.input,
                        tokens_out: cell.output,
                        cache_read: cell.cache_read,
                        cache_creation: cell.cache_creation,
                        cost_usd: crate::pricing::cost_usd(
                            group,
                            model,
                            &tokens,
                            &meta.pricing_overrides,
                        ),
                    }
                })
                .collect();
            models.sort_by(|a, b| {
                (b.tokens_in + b.tokens_out + b.cache_read + b.cache_creation)
                    .cmp(&(a.tokens_in + a.tokens_out + a.cache_read + a.cache_creation))
            });
            TenantUsageDoc {
                tenant: id.clone(),
                name: key.map(|k| k.name.clone()).unwrap_or_else(|| id.clone()),
                email: key.and_then(|k| k.email.clone()),
                requests: t.totals.requests,
                ok: t.totals.ok,
                errors: t.totals.errors,
                tokens_in: t.totals.tokens_in,
                tokens_out: t.totals.tokens_out,
                cost_usd: models.iter().map(|m| m.cost_usd).sum(),
                first_ms: t.first_ms,
                last_ms: t.last_ms,
                models,
            }
        })
        .collect();
    tenant_usage.sort_by(|a, b| {
        b.requests
            .cmp(&a.requests)
            .then((b.tokens_in + b.tokens_out).cmp(&(a.tokens_in + a.tokens_out)))
            .then(a.tenant.cmp(&b.tenant))
    });

    DashboardDoc {
        version: crate::build_info::version_string(),
        pid: meta.pid,
        uptime_secs: meta.uptime_secs,
        port: meta.port,
        current: snapshot.representative_current().map(|c| c.0.clone()),
        current_by_group: snapshot
            .current
            .iter()
            .map(|(group, id)| (group.as_str().to_string(), id.0.clone()))
            .collect(),
        upstream: meta.upstream.clone(),
        config_facts: meta.config_facts.clone(),
        config_path: meta.config_path.clone(),
        select_params: SelectParamsDoc::from(params),
        refresh_ahead_secs: meta.refresh_ahead_secs,
        evaluate_tick_secs: meta.evaluate_tick_secs,
        accounts,
        scheduler,
        poller,
        totals: GlobalTotalsDoc {
            requests: hub.global_totals.requests,
            ok: hub.global_totals.ok,
            errors: hub.global_totals.errors,
            tokens_in: hub.global_totals.tokens_in,
            tokens_out: hub.global_totals.tokens_out,
            rpm_5m: hub.rpm_5m,
            in_flight: in_flight_total,
            cost_usd: total_cost_usd,
        },
        model_usage,
        client_usage,
        tenant_usage,
        client_keys: meta.client_keys.clone(),
        windowed: windowed_docs(hub),
        activity,
        health: Some(HealthDoc {
            requests: hub.health.requests,
            errors: hub.health.errors,
            s429: hub.health.s429,
            s401: hub.health.s401,
            s5xx: hub.health.s5xx,
        }),
        session_labels: hub
            .session_labels
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        logs: hub
            .logs
            .iter()
            .map(|line| LogLineDoc {
                level: line.level.to_string(),
                text: line.text.clone(),
            })
            .collect(),
        codex: meta.codex.clone(),
        grok: meta.grok.clone(),
        daily_usage: hub.daily_usage.clone(),
        daily_perf: hub.daily_perf.clone(),
        usage_stats,
        email_anonymous: meta.email_anonymous,
        tui_effects: meta.tui_effects,
        tui_gradient: meta.tui_gradient.clone(),
        show_fable_weekly: meta.show_fable_weekly,
        domain_abbrev: meta.domain_abbrev.clone(),
        quota_display: meta.quota_display,
        // Canonical label wording (issue #62 S2) — constant per build, not
        // state-derived; `Default` IS the canonical set.
        data_quality: DataQualityDoc::default(),
        // Live event banners (config `events`), read from the daemon's live
        // holder in `build_doc`; carried so both TUI backends render them.
        events: meta.events.clone(),
    }
}

/// Build the document from live server state — what `GET /llmux/dashboard`
/// serves and what the local TUI renders each frame.
pub(crate) fn build_doc(state: &AppState, now: SystemTime) -> DashboardDoc {
    let snapshot = state.pool.snapshot();
    let params = state.select_params();
    let hub = state.hub.view(now);
    let codex_shape = state.codex.shape();
    let grok_shape = state.grok.shape();
    let meta = DocMeta {
        pid: std::process::id(),
        client_keys: state
            .keys
            .list()
            .iter()
            .map(|k| KeyRowDoc {
                id: k.id.clone(),
                name: k.name.clone(),
                email: k.email.clone(),
                kind: match k.kind {
                    crate::config::ClientKeyKind::Admin => "admin".to_string(),
                    crate::config::ClientKeyKind::Default => "default".to_string(),
                },
                key_prefix: k.key_prefix.clone(),
                suspended: k.suspended,
                created_at_ms: k.created_at_ms,
                revoked_at_ms: k.revoked_at_ms,
            })
            .collect(),
        uptime_secs: state.started.elapsed().as_secs(),
        port: state.bound_port.load(std::sync::atomic::Ordering::Relaxed),
        upstream: state.config.upstream.clone(),
        config_facts: ConfigFactsDoc {
            routing_enabled: state
                .settings_live
                .routing_enabled
                .load(std::sync::atomic::Ordering::Relaxed),
            routing_default_group: state.config.routing.default_group.clone(),
            routing_on_empty_group: state.config.routing.on_empty_group.clone(),
            raw_io_enabled: state
                .settings_live
                .raw_io_enabled
                .load(std::sync::atomic::Ordering::Relaxed),
            raw_io_retention_days: state.config.raw_io.retention_days,
            raw_io_max_body_bytes: state.config.raw_io.max_body_bytes as u64,
            gradient_speed: state.config.tui_gradient.speed,
            codex_upstream: state.config.codex.upstream.clone(),
            proxy_max_request_bytes: state.config.proxy.max_request_bytes as u64,
            raw_io_bytes: persist_file_sizes(
                state.raw_io_path.as_deref(),
                state.activity_log_path.as_deref(),
            )
            .0,
            activity_log_bytes: persist_file_sizes(
                state.raw_io_path.as_deref(),
                state.activity_log_path.as_deref(),
            )
            .1,
        },
        config_path: state.config_path.as_ref().map(|p| p.display().to_string()),
        refresh_ahead_secs: state.config.scheduler.refresh_ahead_secs,
        evaluate_tick_secs: EVALUATE_TICK.as_secs(),
        codex: CodexSettingsDoc {
            available: snapshot
                .accounts
                .iter()
                .any(|a| a.group == crate::routing::BackendGroup::Codex),
            fast: codex_shape.fast,
            model: codex_shape.model,
            effort: codex_shape.effort,
        },
        grok: GrokSettingsDoc {
            available: snapshot
                .accounts
                .iter()
                .any(|a| a.group == crate::routing::BackendGroup::Grok),
            model: grok_shape.model,
            effort: grok_shape.effort,
        },
        // Pricing overrides from the live config's `[pricing]` section
        // (Feature D); empty → built-in default rate table.
        pricing_overrides: state.config.pricing.clone(),
        // Live atomic, not the config snapshot: a `POST /llmux/settings` flip
        // must reflect on the very next frame/poll without restart.
        email_anonymous: state
            .email_anonymous
            .load(std::sync::atomic::Ordering::Relaxed),
        // Config-file display gate, same convention as show_fable_weekly: no
        // runtime toggle / endpoint, so read the loaded config snapshot.
        tui_effects: state
            .settings_live
            .tui_effects
            .load(std::sync::atomic::Ordering::Relaxed),
        tui_gradient: state.config.tui_gradient.clone(),
        // Config-file gate (fable-usage U9a). No runtime toggle / endpoint by
        // design — a default-ON config field is the whole TUI-side ask — so
        // this reads the loaded config snapshot directly.
        show_fable_weekly: state
            .settings_live
            .show_fable_weekly
            .load(std::sync::atomic::Ordering::Relaxed),
        // Config-file display settings, same convention as show_fable_weekly:
        // no runtime endpoint; the TUI `u` key overrides quota_display locally.
        domain_abbrev: state.config.domain_abbrev.clone(),
        quota_display: state.settings_live.quota_display(),
        // Live event holder, not the config snapshot: a `POST /llmux/events`
        // must reflect on the very next frame/poll without restart.
        events: state
            .event_banners
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    };
    dashboard_doc(&snapshot, &hub, &state.totals, &params, now, &meta)
}

#[cfg(test)]
mod tests {
    #[test]
    fn daily_perf_doc_wire_round_trip_preserves_every_field() {
        // Attach parity (perf telemetry v1): the perf rows an attach client
        // parses must be exactly what the daemon serialized — including the
        // three-state `fast` (None must survive, never collapse to false).
        let rows = vec![
            super::DailyPerfDoc {
                day: 20_600,
                group: "codex".into(),
                model: "gpt-5.5".into(),
                fast: None,
                requests: 3,
                ok: 2,
                errors: 1,
                tps_n: 2,
                output_tokens: 100,
                e2e_ms: 2_000,
                measured_n: 1,
                measured_output: 60,
                post_ttft_ms: 500,
                ttfb_n: 2,
                ttfb_ms_sum: 240,
            },
            super::DailyPerfDoc {
                fast: Some(true),
                ..{
                    let mut r = super::DailyPerfDoc {
                        day: 20_601,
                        group: "claude".into(),
                        model: "opus".into(),
                        fast: Some(false),
                        requests: 1,
                        ok: 1,
                        errors: 0,
                        tps_n: 1,
                        output_tokens: 10,
                        e2e_ms: 100,
                        measured_n: 0,
                        measured_output: 0,
                        post_ttft_ms: 0,
                        ttfb_n: 0,
                        ttfb_ms_sum: 0,
                    };
                    r.fast = Some(true);
                    r
                }
            },
        ];
        let json = serde_json::to_string(&rows).expect("serialize");
        let parsed: Vec<super::DailyPerfDoc> = serde_json::from_str(&json).expect("parse");
        assert_eq!(rows, parsed, "wire round-trip is lossless");
        assert_eq!(parsed[0].fast, None, "unknown fast survives the wire");
    }

    use super::*;
    use crate::config::{AccountConfig, AccountCredential};
    use crate::scheduler::headers::{ParsedRateLimitHeaders, WindowReading};
    use crate::scheduler::{AccountId, AccountPool};
    use crate::tui::activity::UNKNOWN_CLIENT;

    const NOW_SECS: u64 = 1_000_000;

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS)
    }

    fn params() -> SelectParams {
        SelectParams {
            five_hour_max: 0.90,
            seven_day_max: 0.99,
            fable_weekly_max: 0.98,
            mode: crate::config::SchedulerMode::Default,
            usage_max_age: Duration::from_secs(600),
        }
    }

    fn meta() -> DocMeta {
        DocMeta {
            client_keys: Vec::new(),
            grok: GrokSettingsDoc::default(),
            pid: 4321,
            uptime_secs: 130,
            port: 3456,
            upstream: "https://api.anthropic.com".into(),
            config_facts: Default::default(),
            config_path: Some("/tmp/llmux.json".into()),
            refresh_ahead_secs: 7 * 3600,
            evaluate_tick_secs: 60,
            codex: CodexSettingsDoc {
                available: true,
                fast: false,
                model: "gpt-5.5".into(),
                effort: None,
            },
            pricing_overrides: HashMap::new(),
            email_anonymous: false,
            tui_effects: true,
            tui_gradient: crate::config::TuiGradient::default(),
            show_fable_weekly: true,
            domain_abbrev: crate::config::default_domain_abbrev(),
            quota_display: crate::config::QuotaDisplay::Used,
            events: Vec::new(),
        }
    }

    fn oauth_account(name: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            credential: AccountCredential::Oauth {
                account_uuid: format!("uuid-{name}"),
                access_token: format!("at-{name}"),
                refresh_token: format!("rt-{name}"),
                expires_at_ms: 0,
                tier: None,
                last_refresh_ms: None,
            },
        }
    }

    fn codex_account(name: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            credential: AccountCredential::Codex {
                account_id: format!("acct-{name}"),
                access_token: format!("at-{name}"),
                refresh_token: format!("rt-{name}"),
                expires_at_ms: 0,
                last_refresh_ms: None,
            },
        }
    }

    /// Hub fed with a realistic event sequence: request lifecycle, a switch,
    /// a usage poll, and a tracing line.
    fn seeded_hub() -> DashboardHub {
        let hub = DashboardHub::default();
        hub.apply_event(
            ActivityEvent::AccountSwitched {
                from: None,
                to: "a".into(),
                reason: Some("initial selection".into()),
            },
            now() - Duration::from_secs(90),
        );
        hub.apply_event(
            ActivityEvent::RequestStarted {
                id: 1,
                method: "POST".into(),
                path: "/v1/messages".into(),
                kind: None,
            },
            now() - Duration::from_secs(60),
        );
        hub.apply_event(
            ActivityEvent::RequestFinished {
                id: 1,
                method: "POST".into(),
                path: "/v1/messages".into(),
                account: Some("a".into()),
                status: 200,
                duration: Duration::from_millis(1_400),
                tokens: Some(TokenCounts {
                    input: 700,
                    output: 300,
                    cache_read: Some(120),
                    cache_creation: None,
                }),
                group: Some("codex".into()),
                model: Some("gpt-5.5".into()),
                effort: Some("high".into()),
                fast: Some(true),
                ttfb_ms: None,
                ttft_ms: None,
                gen_ms: None,
                aborted: false,
                user_id: Some("acct_seed".into()),
                kind: None,
                excerpt: None,
                tenant: None,
            },
            now() - Duration::from_secs(58),
        );
        hub.apply_event(
            ActivityEvent::RequestStarted {
                id: 2,
                method: "POST".into(),
                path: "/v1/messages".into(),
                kind: None,
            },
            now() - Duration::from_secs(3),
        );
        // In-flight request routed to the same codex model — exercises the
        // per-model in-flight overlay (req11).
        hub.apply_event(
            ActivityEvent::RequestRouted {
                id: 2,
                account: "a".into(),
                group: Some("codex".into()),
                model: Some("gpt-5.5".into()),
                effort: Some("xhigh".into()),
                fast: true,
            },
            now() - Duration::from_secs(2),
        );
        hub.apply_event(
            ActivityEvent::UsagePolled {
                account: "a".into(),
                ok: true,
                consecutive_failures: 0,
                next_in: Duration::from_secs(300),
            },
            now() - Duration::from_secs(10),
        );
        hub.push_log(LogLine {
            level: tracing::Level::INFO,
            text: "proxy: proxy listening".into(),
        });
        hub
    }

    fn seeded_doc() -> DashboardDoc {
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        pool.evaluate(None, &params(), now());
        pool.record_headers(
            &AccountId("a".into()),
            &ParsedRateLimitHeaders {
                five_hour: Some(WindowReading {
                    utilization: 0.42,
                    resets_at: now() + Duration::from_secs(3600),
                }),
                seven_day: Some(WindowReading {
                    utilization: 0.10,
                    resets_at: now() + Duration::from_secs(86_400),
                }),
                ..Default::default()
            },
            now(),
        );
        pool.record_429(
            &AccountId("b".into()),
            Some(Duration::from_secs(120)),
            now(),
        );
        let totals = UsageTotals::default();
        totals.record(&AccountId("a".into()), 1, 700, 300);
        let hub = seeded_hub();
        dashboard_doc(
            &pool.snapshot(),
            &hub.view(now()),
            &totals,
            &params(),
            now(),
            &meta(),
        )
    }

    /// Multi-tenant #22: the doc's tenant rows join hub aggregates with key
    /// metadata (name/email), price the per-model cells, and carry the
    /// first/last-seen span. Unknown ids (pre-tenant history) name themselves.
    #[test]
    fn doc_tenant_rows_are_priced_named_and_spanned() {
        let hub = DashboardHub::default();
        let finished = |id: u64, tenant: Option<&str>, at: SystemTime| {
            (
                ActivityEvent::RequestFinished {
                    id,
                    method: "POST".into(),
                    path: "/v1/messages".into(),
                    account: Some("a".into()),
                    status: 200,
                    duration: Duration::from_millis(900),
                    tokens: Some(TokenCounts {
                        input: 1_000_000,
                        output: 0,
                        cache_read: None,
                        cache_creation: None,
                    }),
                    group: Some("claude".into()),
                    model: Some("claude-opus-4-8".into()),
                    effort: None,
                    fast: Some(false),
                    ttfb_ms: None,
                    ttft_ms: None,
                    gen_ms: None,
                    aborted: false,
                    user_id: None,
                    kind: None,
                    excerpt: None,
                    tenant: tenant.map(str::to_string),
                },
                at,
            )
        };
        let (e, at) = finished(1, Some("k-t1"), now() - Duration::from_secs(500));
        hub.apply_event(e, at);
        let (e, at) = finished(2, Some("k-t1"), now() - Duration::from_secs(50));
        hub.apply_event(e, at);
        let (e, at) = finished(3, None, now() - Duration::from_secs(10));
        hub.apply_event(e, at);

        let mut meta = meta();
        meta.client_keys.push(KeyRowDoc {
            id: "k-t1".into(),
            name: "pc-a".into(),
            email: Some("a@x.com".into()),
            kind: "default".into(),
            key_prefix: "lmk-aaaa".into(),
            suspended: false,
            created_at_ms: 1,
            revoked_at_ms: None,
        });
        let pool = AccountPool::new(&[oauth_account("a")]);
        let doc = dashboard_doc(
            &pool.snapshot(),
            &hub.view(now()),
            &UsageTotals::default(),
            &params(),
            now(),
            &meta,
        );

        assert_eq!(doc.tenant_usage.len(), 2, "keyed tenant + unknown bucket");
        let t1 = &doc.tenant_usage[0];
        assert_eq!(t1.tenant, "k-t1");
        assert_eq!(t1.name, "pc-a", "name joined from key metadata");
        assert_eq!(t1.email.as_deref(), Some("a@x.com"));
        assert_eq!(t1.requests, 2);
        assert_eq!(t1.tokens_in, 2_000_000);
        // Span covers both requests.
        assert_eq!(t1.first_ms, (NOW_SECS - 500) * 1000);
        assert_eq!(t1.last_ms, (NOW_SECS - 50) * 1000);
        // Priced per-model breakdown: 2M input tokens of opus have a real
        // API-equivalent cost (built-in rate table, no overrides).
        assert_eq!(t1.models.len(), 1);
        assert_eq!(t1.models[0].group, "claude");
        assert_eq!(t1.models[0].requests, 2);
        assert!(t1.models[0].cost_usd > 0.0, "priced from the rate table");
        assert!((t1.cost_usd - t1.models[0].cost_usd).abs() < f64::EPSILON);
        // Pre-tenant history: named after its bucket, never a live one.
        let unk = &doc.tenant_usage[1];
        assert_eq!(unk.tenant, UNKNOWN_CLIENT);
        assert_eq!(unk.name, UNKNOWN_CLIENT);
        // The doc's key list is metadata-only (no secret material fields).
        assert_eq!(doc.client_keys.len(), 1);
        let serialized = serde_json::to_string(&doc.client_keys).expect("json");
        assert!(!serialized.contains("digest"));
    }

    /// Activity client-name: every completed request row carries its tenant
    /// id plus the resolved display name — key name for `k-…` ids, the
    /// bucket id itself for builtins (`local`), and `None` (skipped on the
    /// wire) for pre-tenant history — never coerced into `local`.
    #[test]
    fn doc_completed_rows_carry_tenant_and_resolved_client_name() {
        let hub = DashboardHub::default();
        let finished = |id: u64, tenant: Option<&str>, at: SystemTime| {
            (
                ActivityEvent::RequestFinished {
                    id,
                    method: "POST".into(),
                    path: "/v1/messages".into(),
                    account: Some("a".into()),
                    status: 200,
                    duration: Duration::from_millis(900),
                    tokens: None,
                    group: Some("claude".into()),
                    model: Some("claude-opus-4-8".into()),
                    effort: None,
                    fast: Some(false),
                    ttfb_ms: None,
                    ttft_ms: None,
                    gen_ms: None,
                    aborted: false,
                    user_id: None,
                    kind: None,
                    excerpt: None,
                    tenant: tenant.map(str::to_string),
                },
                at,
            )
        };
        let (e, at) = finished(1, Some("k-t1"), now() - Duration::from_secs(30));
        hub.apply_event(e, at);
        let (e, at) = finished(2, Some("local"), now() - Duration::from_secs(20));
        hub.apply_event(e, at);
        let (e, at) = finished(3, None, now() - Duration::from_secs(10));
        hub.apply_event(e, at);

        let mut meta = meta();
        meta.client_keys.push(KeyRowDoc {
            id: "k-t1".into(),
            name: "Z (U09F1M5MML1)".into(),
            email: None,
            kind: "default".into(),
            key_prefix: "lmk-aaaa".into(),
            suspended: false,
            created_at_ms: 1,
            revoked_at_ms: None,
        });
        let pool = AccountPool::new(&[oauth_account("a")]);
        let doc = dashboard_doc(
            &pool.snapshot(),
            &hub.view(now()),
            &UsageTotals::default(),
            &params(),
            now(),
            &meta,
        );

        let row = |i: usize| match &doc.activity.completed[i] {
            CompletedDoc::Request {
                tenant,
                client_name,
                ..
            } => (tenant.clone(), client_name.clone()),
            other => panic!("expected request, got {other:?}"),
        };
        // Newest first: [0] = pre-tenant, [1] = local, [2] = keyed.
        assert_eq!(row(0), (None, None), "None stays None, never `local`");
        assert_eq!(
            row(1),
            (Some("local".into()), Some("local".into())),
            "builtin buckets name themselves"
        );
        assert_eq!(
            row(2),
            (Some("k-t1".into()), Some("Z (U09F1M5MML1)".into()),),
            "key name joined from key metadata"
        );
        // Additive wire shape: the pre-tenant row omits both keys entirely.
        let json = serde_json::to_value(&doc.activity.completed[0]).expect("json");
        assert!(json.get("tenant").is_none());
        assert!(json.get("client_name").is_none());
    }

    #[test]
    fn doc_is_a_status_superset_with_accounts_in_selection_order() {
        let doc = seeded_doc();
        assert!(doc.version.starts_with("llmux "));
        assert_eq!(doc.pid, 4321);
        assert_eq!(doc.port, 3456);
        assert_eq!(doc.uptime_secs, 130);
        assert_eq!(doc.current.as_deref(), Some("a"));
        assert_eq!(doc.upstream, "https://api.anthropic.com");
        assert_eq!(doc.config_path.as_deref(), Some("/tmp/llmux.json"));

        // Selection order: current first, parked account last.
        let names: Vec<&str> = doc.accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert_eq!(doc.accounts[0].order, 1);
        assert_eq!(doc.accounts[0].status, "active");
        assert!(doc.accounts[0].healthy);
        assert_eq!(doc.accounts[1].status, "cooldown");
        assert_eq!(doc.accounts[1].blocked.as_deref(), Some("cooldown 2m00s"));
        assert_eq!(
            doc.accounts[1].cooldown_source.as_deref(),
            Some("retry_after")
        );

        // Window carries the reconstruction fields.
        let five = doc.accounts[0].five_hour.as_ref().expect("5h window");
        assert!((five.utilization - 0.42).abs() < 1e-9);
        assert_eq!(five.resets_at, NOW_SECS + 3600);
        assert_eq!(five.resets_in_secs, 3600);
        assert_eq!(five.fetched_at_ms, NOW_SECS * 1000);
        assert_eq!(five.source, "headers");

        // Lifetime (proxy) + session (activity) totals both present.
        assert_eq!(doc.accounts[0].totals.requests, 1);
        assert_eq!(doc.accounts[0].totals.input_tokens, 700);
        assert_eq!(doc.accounts[0].session.requests, 1);
        assert_eq!(doc.accounts[0].session.ok, 1);
        assert_eq!(doc.accounts[0].session.tokens_out, 300);
    }

    #[test]
    fn doc_surfaces_fable_weekly_and_scoped_limits_and_round_trips() {
        // A Fable (`limits[]` weekly_scoped) window recorded via the usage path
        // reaches the AccountDoc as `fable_weekly` + a generic `scoped_limits`
        // entry, and the whole doc survives a JSON round-trip (guards the
        // flattened `scoped_limits` shape). Account "b", never polled, proves the
        // null / empty case.
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        pool.evaluate(None, &params(), now());
        let usage = crate::scheduler::usage::parse_usage_body(
            crate::scheduler::usage::DEV1_USAGE_FIXTURE.as_bytes(),
        )
        .expect("fixture parses");
        pool.record_usage(&AccountId("a".into()), &usage, now());
        let doc = dashboard_doc(
            &pool.snapshot(),
            &seeded_hub().view(now()),
            &UsageTotals::default(),
            &params(),
            now(),
            &meta(),
        );

        let a = doc
            .accounts
            .iter()
            .find(|acc| acc.name == "a")
            .expect("account a");
        let b = doc
            .accounts
            .iter()
            .find(|acc| acc.name == "b")
            .expect("account b");

        let fable = a.fable_weekly.as_ref().expect("fable_weekly present");
        assert!((fable.utilization - 1.0).abs() < 1e-9);
        assert_eq!(fable.severity, "critical");
        assert!(fable.is_active);
        assert_eq!(fable.resets_at, 1_783_115_999);
        // Reset-aware `constraining`: this fixture window is critical AND live
        // (resets_at 1_783_115_999 ≫ now 1_000_000, so NOT expired), so
        // `is_constraining` returns true — unambiguously distinct from the
        // stale-severity/expired case the field exists to disambiguate.
        assert!(
            fable.constraining,
            "critical + non-expired window is constraining"
        );
        assert_eq!(a.scoped_limits.len(), 1);
        assert_eq!(a.scoped_limits[0].scope_label, "Fable");
        assert_eq!(a.scoped_limits[0].window.severity, "critical");
        assert!(a.scoped_limits[0].window.constraining);

        assert!(b.fable_weekly.is_none(), "no poll → no Fable window");
        assert!(b.scoped_limits.is_empty());

        // JSON round-trip: fable_weekly is a flat object without scope_label;
        // each scoped_limits entry flattens scope_label alongside the window
        // fields; and a re-parsed doc keeps the same values.
        let json: serde_json::Value = serde_json::to_value(&doc).expect("doc serializes");
        let a_json = json["accounts"]
            .as_array()
            .expect("accounts")
            .iter()
            .find(|acc| acc["name"] == "a")
            .expect("a json");
        assert!(a_json["fable_weekly"].get("scope_label").is_none());
        assert_eq!(a_json["fable_weekly"]["is_active"], true);
        // The reset-aware `constraining` bool serializes under its own JSON key.
        assert_eq!(a_json["fable_weekly"]["constraining"], true);
        assert_eq!(a_json["scoped_limits"][0]["scope_label"], "Fable");
        assert_eq!(a_json["scoped_limits"][0]["is_active"], true);
        assert_eq!(a_json["scoped_limits"][0]["constraining"], true);
        let reparsed: DashboardDoc = serde_json::from_value(json).expect("doc deserializes");
        let a2 = reparsed
            .accounts
            .iter()
            .find(|acc| acc.name == "a")
            .expect("reparsed a");
        assert_eq!(a2.scoped_limits[0].scope_label, "Fable");
        assert!(a2.fable_weekly.as_ref().expect("fable").is_active);
        // `constraining` survives the serialize → deserialize round-trip.
        assert!(a2.fable_weekly.as_ref().expect("fable").constraining);
        assert!(a2.scoped_limits[0].window.constraining);
    }

    #[test]
    fn doc_carries_per_group_current_slots() {
        // Routing on: claude and codex each pick a current independently, so
        // the doc must carry BOTH slots — not just the representative scalar.
        let pool = AccountPool::new(&[oauth_account("a"), codex_account("c")]);
        pool.evaluate(Some(crate::routing::BackendGroup::Claude), &params(), now());
        pool.evaluate(Some(crate::routing::BackendGroup::Codex), &params(), now());
        let doc = dashboard_doc(
            &pool.snapshot(),
            &seeded_hub().view(now()),
            &UsageTotals::default(),
            &params(),
            now(),
            &meta(),
        );
        // Representative scalar stays the claude slot (back-compat).
        assert_eq!(doc.current.as_deref(), Some("a"));
        // The new per-group map carries both group currents.
        assert_eq!(
            doc.current_by_group.get("claude").map(String::as_str),
            Some("a")
        );
        assert_eq!(
            doc.current_by_group.get("codex").map(String::as_str),
            Some("c")
        );
    }

    #[test]
    fn doc_carries_scheduler_poller_totals_activity_and_log_tails() {
        let doc = seeded_doc();

        let switch = doc.scheduler.last_switch.as_ref().expect("last switch");
        assert_eq!(switch.to, "a");
        assert_eq!(switch.reason.as_deref(), Some("initial selection"));
        assert_eq!(switch.at_ms, (NOW_SECS - 90) * 1000);
        assert_eq!(
            doc.scheduler.next_in_line, None,
            "b is parked — nothing eligible besides current"
        );
        assert_eq!(doc.scheduler.next_eval_in_secs, 60 - (130 % 60));

        assert_eq!(doc.poller.len(), 1);
        assert_eq!(doc.poller[0].account, "a");
        assert_eq!(doc.poller[0].last_ok_ms, Some((NOW_SECS - 10) * 1000));
        assert_eq!(doc.poller[0].consecutive_failures, 0);
        assert_eq!(doc.poller[0].next_at_ms, (NOW_SECS - 10 + 300) * 1000);

        assert_eq!(doc.totals.requests, 1);
        assert_eq!(doc.totals.ok, 1);
        assert_eq!(doc.totals.errors, 0);
        assert_eq!(doc.totals.tokens_in, 700);
        assert_eq!(doc.totals.tokens_out, 300);

        // Activity: one in-flight (id 2), completed request + switch note.
        assert_eq!(doc.activity.in_flight.len(), 1);
        assert_eq!(doc.activity.in_flight[0].id, 2);
        assert_eq!(doc.activity.in_flight[0].path, "/v1/messages");
        // Routed effort/fast ride the in-flight doc so the running badge
        // matches the eventual completed badge.
        assert_eq!(doc.activity.in_flight[0].effort.as_deref(), Some("xhigh"));
        assert!(doc.activity.in_flight[0].fast);
        assert!(matches!(
            &doc.activity.completed[0],
            CompletedDoc::Request {
                status: 200,
                duration_ms: 1400,
                ..
            }
        ));
        // group/model/effort/fast (req7) are carried into the doc.
        match &doc.activity.completed[0] {
            CompletedDoc::Request {
                group,
                model,
                effort,
                fast,
                tokens,
                ..
            } => {
                assert_eq!(group.as_deref(), Some("codex"));
                assert_eq!(model.as_deref(), Some("gpt-5.5"));
                assert_eq!(effort.as_deref(), Some("high"));
                assert!(
                    matches!(fast, Some(true)),
                    "codex fast mode is carried into the doc"
                );
                // The full token split rides the doc — cache counters included,
                // so the ATTACH-mode detail row is not permanently `—`.
                let t = tokens.expect("tokens");
                assert_eq!(t.cache_read, Some(120));
                assert_eq!(t.cache_creation, None, "unreported stays None");
            }
            other => panic!("expected request, got {other:?}"),
        }
        assert!(doc
            .activity
            .completed
            .iter()
            .any(|e| matches!(e, CompletedDoc::Note { text, .. } if text.contains("switch"))));

        assert_eq!(doc.logs.len(), 1);
        assert_eq!(doc.logs[0].level, "INFO");
        assert!(doc.logs[0].text.contains("proxy listening"));
    }

    #[test]
    fn tokens_doc_cache_fields_are_additive_and_round_trip() {
        // Docs written before the cache split existed deserialize to None.
        let old: TokensDoc = serde_json::from_str(r#"{"input":70,"output":30}"#).expect("old doc");
        assert_eq!(old.cache_read, None);
        assert_eq!(old.cache_creation, None);
        // Round-trip preserves the split; absent counters are omitted on the
        // wire (never serialized as null/0).
        let json = serde_json::to_string(&TokensDoc {
            input: 1,
            output: 2,
            cache_read: Some(3),
            cache_creation: None,
        })
        .expect("serialize");
        assert!(json.contains(r#""cache_read":3"#));
        assert!(!json.contains("cache_creation"));
        let back: TokensDoc = serde_json::from_str(&json).expect("round-trip");
        assert_eq!(back.cache_read, Some(3));
        assert_eq!(back.cache_creation, None);
    }

    #[test]
    fn doc_carries_model_usage_rows_with_cache_breakdowns_and_in_flight() {
        let doc = seeded_doc();
        // One finished codex/gpt-5.5 request + one in-flight routed to it.
        assert_eq!(doc.model_usage.len(), 1);
        let row = &doc.model_usage[0];
        assert_eq!(row.group, "codex");
        assert_eq!(row.model, "gpt-5.5");
        assert_eq!(row.requests, 1);
        assert_eq!(row.ok, 1);
        assert_eq!(row.errors, 0);
        assert_eq!(row.tokens_in, 700);
        assert_eq!(row.tokens_out, 300);
        // cache_read captured; cache_creation never reported → omitted.
        assert_eq!(row.cache_read, Some(120));
        assert_eq!(row.cache_creation, None);
        assert_eq!(row.last_used_ms, (NOW_SECS - 58) * 1000);
        // The routed-but-unfinished request overlays as in-flight (req11).
        assert_eq!(row.in_flight, 1);
        // Breakdowns.
        assert_eq!(row.accounts.len(), 1);
        assert_eq!(row.accounts[0].name, "a");
        assert_eq!(row.accounts[0].tokens_in, 700);
        assert_eq!(
            row.efforts
                .iter()
                .find(|e| e.label == "high")
                .map(|e| e.requests),
            Some(1)
        );
        assert_eq!(
            row.endpoints
                .iter()
                .find(|e| e.label == "messages")
                .map(|e| e.requests),
            Some(1)
        );
    }

    #[test]
    fn doc_carries_usage_calendar_rows_priced_and_round_trips() {
        let doc = seeded_doc();
        // One finished codex/gpt-5.5 request → exactly one row per
        // granularity (hour, day, month), each carrying the same counters.
        assert_eq!(doc.usage_stats.len(), 3, "hour + day + month rows");
        for gran in ["hour", "day", "month"] {
            let row = doc
                .usage_stats
                .iter()
                .find(|r| r.gran == gran)
                .unwrap_or_else(|| panic!("missing {gran} row"));
            assert_eq!(
                (row.group.as_str(), row.model.as_str()),
                ("codex", "gpt-5.5")
            );
            assert_eq!(row.requests, 1);
            assert_eq!(row.tokens_in, 700);
            assert_eq!(row.tokens_out, 300);
            assert_eq!(row.cache_read, 120);
            assert_eq!(row.cache_creation, 0);
            assert!(!row.label.is_empty(), "server-rendered label present");
            // Priced server-side (T6) at the built-in gpt-5.5 rates: the doc
            // build and a direct pricing call must agree exactly.
            let want = crate::pricing::cost_from_parts(
                "codex",
                "gpt-5.5",
                700,
                300,
                Some(120),
                Some(0),
                &HashMap::new(),
            );
            assert!(want > 0.0, "seeded model has a nonzero rate");
            assert!((row.cost_usd - want).abs() < 1e-12, "doc cost == pricing");
            assert!(row.priced, "known (group, model) is marked priced");
        }
        // Round-trip: rows survive serialization (attach mode parses these).
        let json = serde_json::to_string(&doc).expect("serialize");
        let back: DashboardDoc = serde_json::from_str(&json).expect("parse");
        assert_eq!(back.usage_stats, doc.usage_stats);
        // Additive contract: a document from an OLDER daemon (field absent)
        // parses to an empty list, never an error.
        let mut value: serde_json::Value = serde_json::to_value(&doc).expect("to_value");
        value.as_object_mut().expect("object").remove("usage_stats");
        let old: DashboardDoc = serde_json::from_value(value).expect("parse older doc");
        assert!(old.usage_stats.is_empty());
    }

    #[test]
    fn cache_creation_omitted_from_json_when_unavailable() {
        let doc = seeded_doc();
        let value: serde_json::Value = serde_json::to_value(&doc).expect("serialize");
        let row = &value["model_usage"][0];
        assert_eq!(row["cache_read"], 120);
        // None → skipped entirely, so the client renders "unavailable" not 0.
        assert!(row.get("cache_creation").is_none());
    }

    /// Same seed as [`seeded_doc`] but with a caller-supplied [`DocMeta`] so a
    /// test can inject pricing overrides (Feature D).
    fn seeded_doc_with_meta(meta: &DocMeta) -> DashboardDoc {
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        pool.evaluate(None, &params(), now());
        let totals = UsageTotals::default();
        totals.record(&AccountId("a".into()), 1, 700, 300);
        let hub = seeded_hub();
        dashboard_doc(
            &pool.snapshot(),
            &hub.view(now()),
            &totals,
            &params(),
            now(),
            meta,
        )
    }

    #[test]
    fn doc_carries_events_from_meta_and_skips_them_when_empty() {
        // Present: the meta's live-holder events land in the doc and survive a
        // JSON round-trip verbatim.
        let mut meta = meta();
        meta.events = vec![crate::config::EventBanner {
            id: "20260712-fable5".into(),
            from: "202607080000".into(),
            to: "202607130000".into(),
            content: "Fable 5 Available until 7/12".into(),
        }];
        let doc = seeded_doc_with_meta(&meta);
        assert_eq!(doc.events.len(), 1, "meta events carried into doc");
        assert_eq!(doc.events[0].id, "20260712-fable5");
        let json: serde_json::Value = serde_json::to_value(&doc).expect("serialize");
        assert_eq!(json["events"][0]["id"], "20260712-fable5");
        assert_eq!(json["events"][0]["content"], "Fable 5 Available until 7/12");
        let reparsed: DashboardDoc = serde_json::from_value(json).expect("deserialize");
        assert_eq!(reparsed.events, doc.events);

        // Empty: `meta()` defaults `events: []`, so the additive
        // `skip_serializing_if` keeps the field off the wire entirely — an
        // older client never sees an unexpected key.
        let doc_none = seeded_doc();
        assert!(doc_none.events.is_empty());
        let json_none: serde_json::Value = serde_json::to_value(&doc_none).expect("serialize");
        assert!(
            json_none.get("events").is_none(),
            "events omitted from the wire when empty"
        );
    }

    #[test]
    fn doc_carries_api_equivalent_cost_with_default_pricing() {
        // The single completed request is codex/gpt-5.5 with input=700,
        // output=300, cache_read=120. Under the built-in gpt-5.5 rates
        // {5.0, 30.0, 0.5, 0.0} per 1e6: 0.0035 + 0.009 + 0.00006 = 0.01256.
        let tokens = TokenCounts {
            input: 700,
            output: 300,
            cache_read: Some(120),
            cache_creation: None,
        };
        let expected = crate::pricing::cost_usd("codex", "gpt-5.5", &tokens, &HashMap::new());

        let doc = seeded_doc();
        // Global total = sum of per-model row costs (here, the one codex row).
        assert!(
            (doc.totals.cost_usd - expected).abs() < 1e-9,
            "global cost {} != {expected}",
            doc.totals.cost_usd
        );
        assert!(
            (expected - 0.012_56).abs() < 1e-9,
            "sanity: expected gpt-5.5 cost is 0.01256, got {expected}"
        );

        // Per-request activity line carries the same cost.
        match &doc.activity.completed[0] {
            CompletedDoc::Request { cost_usd, .. } => {
                assert!(
                    (*cost_usd - expected).abs() < 1e-9,
                    "per-request cost {cost_usd} != {expected}"
                );
            }
            other => panic!("expected request, got {other:?}"),
        }
    }

    #[test]
    fn doc_global_cost_reflects_config_pricing_override() {
        // Override gpt-5.5 input to 9.99/1e6, everything else 0 → cost is
        // purely 700 * 9.99 / 1e6 = 0.006993 for the one request.
        let mut overrides = HashMap::new();
        overrides.insert(
            "gpt-5.5".to_string(),
            crate::pricing::ModelPrice {
                input: 9.99,
                output: 0.0,
                cache_read: 0.0,
                cache_creation: 0.0,
            },
        );
        let mut m = meta();
        m.pricing_overrides = overrides.clone();

        let doc = seeded_doc_with_meta(&m);
        let expected = 700.0 * 9.99 / 1_000_000.0;
        assert!(
            (doc.totals.cost_usd - expected).abs() < 1e-9,
            "override global cost {} != {expected}",
            doc.totals.cost_usd
        );
        // And the default (no override) gives a different number, proving the
        // override actually took effect.
        let default_doc = seeded_doc();
        assert!(
            (default_doc.totals.cost_usd - doc.totals.cost_usd).abs() > 1e-6,
            "override must change the cost from the default"
        );
    }

    #[test]
    fn doc_cost_round_trips_through_json() {
        let doc = seeded_doc();
        let value: serde_json::Value = serde_json::to_value(&doc).expect("serialize");
        // Global cost is a plain f64 field on totals.
        assert!(value["totals"]["cost_usd"].is_number());
        // Per-request cost is serialized on the completed request entry.
        let completed = value["activity"]["completed"]
            .as_array()
            .expect("completed array");
        let req = completed
            .iter()
            .find(|e| e["kind"] == "request")
            .expect("a request entry");
        assert!(req["cost_usd"].is_number());

        let parsed: DashboardDoc = serde_json::from_value(value).expect("parse");
        assert!((parsed.totals.cost_usd - doc.totals.cost_usd).abs() < 1e-12);
    }

    #[test]
    fn doc_without_cost_field_parses_to_zero() {
        // A pre-Feature-D document omits cost_usd; the additive serde default
        // keeps it parseable (0.0) so an upgraded client can still attach.
        let doc = seeded_doc();
        let mut value = serde_json::to_value(&doc).expect("serialize");
        value["totals"].as_object_mut().unwrap().remove("cost_usd");
        let parsed: DashboardDoc = serde_json::from_value(value).expect("parse");
        assert_eq!(parsed.totals.cost_usd, 0.0);
    }

    #[test]
    fn doc_carries_windowed_heatmap_cells_per_window() {
        // The seeded hub folds one finished codex/gpt-5.5 request on account
        // "a" (issue #23): both the 24h and 72h windows must carry a cell for
        // (codex, gpt-5.5, a) with the right counts.
        let doc = seeded_doc();
        // One slice per retained window.
        assert_eq!(doc.windowed.len(), 2);
        let labels: Vec<&str> = doc.windowed.iter().map(|w| w.window.as_str()).collect();
        assert_eq!(labels, vec!["24h", "72h"]);

        for slice in &doc.windowed {
            let cell = slice
                .cells
                .iter()
                .find(|c| c.group == "codex" && c.model == "gpt-5.5" && c.account == "a")
                .unwrap_or_else(|| panic!("missing cell in {} window", slice.window));
            assert_eq!(cell.requests, 1);
            assert_eq!(cell.ok, 1);
            assert_eq!(cell.errors, 0);
            assert_eq!(cell.tokens_in, 700);
            assert_eq!(cell.tokens_out, 300);
            assert_eq!(cell.cache_read, 120);
            // tokens() intensity = in + out + cache_read + cache_creation.
            assert_eq!(cell.tokens, 700 + 300 + 120);
        }
    }

    #[test]
    fn doc_without_windowed_field_parses_to_empty() {
        // A pre-#23 daemon's document predates `windowed` — the additive serde
        // default keeps it parseable so an upgraded client can still attach.
        let doc = seeded_doc();
        let mut value = serde_json::to_value(&doc).expect("serialize");
        value.as_object_mut().unwrap().remove("windowed");
        let parsed: DashboardDoc = serde_json::from_value(value).expect("parse");
        assert!(parsed.windowed.is_empty());
    }

    #[test]
    fn doc_without_model_usage_field_parses_to_empty() {
        // An older daemon's document predates `model_usage` — additive default
        // keeps it parseable so an upgraded client can still attach (req23/33).
        let doc = seeded_doc();
        let mut value = serde_json::to_value(&doc).expect("serialize");
        value.as_object_mut().unwrap().remove("model_usage");
        let parsed: DashboardDoc = serde_json::from_value(value).expect("parse");
        assert!(parsed.model_usage.is_empty());
    }

    #[test]
    fn doc_carries_email_anonymous_and_defaults_false_for_old_docs() {
        // The doc surfaces the live setting (from DocMeta), keeps names REAL
        // (T1), and an older daemon's doc (no field) parses to false so the
        // attach client falls back to no masking.
        let mut m = meta();
        m.email_anonymous = true;
        let doc = seeded_doc_with_meta(&m);
        assert!(doc.email_anonymous);
        assert_eq!(doc.accounts[0].name, "a", "doc names stay real");

        let mut value = serde_json::to_value(&doc).expect("serialize");
        assert_eq!(value["email_anonymous"], true, "carried on the wire");
        value.as_object_mut().unwrap().remove("email_anonymous");
        let parsed: DashboardDoc = serde_json::from_value(value).expect("parse");
        assert!(!parsed.email_anonymous, "older daemon → false");
    }

    #[test]
    fn doc_without_cost_fields_parses_to_zero_defaults() {
        // An older daemon's doc predates `ModelUsageDoc::cost_usd` and the
        // `ClientUsageDoc` cost_usd/last_seen_ms fields (issue #62 S1) —
        // additive `#[serde(default)]` keeps it parseable, all three read 0.
        let doc = seeded_doc();
        let mut value = serde_json::to_value(&doc).expect("serialize");
        for row in value["model_usage"].as_array_mut().expect("model rows") {
            row.as_object_mut().unwrap().remove("cost_usd");
        }
        for row in value["client_usage"].as_array_mut().expect("client rows") {
            let obj = row.as_object_mut().unwrap();
            obj.remove("cost_usd");
            obj.remove("last_seen_ms");
        }
        let parsed: DashboardDoc = serde_json::from_value(value).expect("parse");
        // Non-vacuous: the seeded doc has rows on both surfaces.
        assert!(!parsed.model_usage.is_empty());
        assert!(!parsed.client_usage.is_empty());
        assert!(parsed.model_usage.iter().all(|m| m.cost_usd.abs() < 1e-12));
        assert!(parsed
            .client_usage
            .iter()
            .all(|c| c.cost_usd.abs() < 1e-12 && c.last_seen_ms == 0));
    }

    #[test]
    fn doc_round_trips_cost_fields() {
        // The new additive fields survive serialize→parse (issue #62 S1) and
        // are PRESENT on the wire (no skip_serializing), so a newer client
        // can rely on the keys existing in a fresh daemon's doc.
        let doc = seeded_doc();
        let json = serde_json::to_string(&doc).expect("serialize");
        let parsed: DashboardDoc = serde_json::from_str(&json).expect("parse");
        assert!(doc.model_usage[0].cost_usd > 0.0, "seeded row is priced");
        assert!((parsed.model_usage[0].cost_usd - doc.model_usage[0].cost_usd).abs() < 1e-12);

        let value: serde_json::Value = serde_json::from_str(&json).expect("value");
        let client = &value["client_usage"][0];
        assert!(client.get("cost_usd").is_some(), "on the wire");
        assert!(client.get("last_seen_ms").is_some(), "on the wire");
        // Wire-ready defaults until the hub tracks per-client cost/last-seen.
        assert!(parsed.client_usage[0].cost_usd.abs() < 1e-12);
        assert_eq!(parsed.client_usage[0].last_seen_ms, 0);
    }

    #[test]
    fn doc_without_data_quality_field_parses_to_canonical_labels() {
        // An older daemon's doc predates `data_quality` (issue #62 S2) — the
        // additive serde default fills the EXACT canonical wording, so a
        // client renders the same bytes whether or not the field was on the
        // wire (the old-daemon fallback IS the default, by construction).
        let doc = seeded_doc();
        let mut value = serde_json::to_value(&doc).expect("serialize");
        value.as_object_mut().unwrap().remove("data_quality");
        let parsed: DashboardDoc = serde_json::from_value(value).expect("parse");
        assert_eq!(parsed.data_quality.model_usage, "hydrated activity/runtime");
        assert_eq!(parsed.data_quality.windowed, "best effort");
        assert_eq!(parsed.data_quality.cost, "API-equivalent estimate");
        assert_eq!(
            parsed.data_quality.cache,
            "missing fields shown as unavailable"
        );
    }

    #[test]
    fn doc_round_trips_data_quality_labels() {
        // A fresh daemon puts the canonical labels ON the wire (no
        // skip_serializing — clients may rely on the keys existing) and they
        // survive serialize→parse unchanged.
        let doc = seeded_doc();
        let value = serde_json::to_value(&doc).expect("serialize");
        assert_eq!(
            value["data_quality"]["model_usage"],
            "hydrated activity/runtime"
        );
        assert_eq!(value["data_quality"]["windowed"], "best effort");
        assert_eq!(value["data_quality"]["cost"], "API-equivalent estimate");
        assert_eq!(
            value["data_quality"]["cache"],
            "missing fields shown as unavailable"
        );
        let parsed: DashboardDoc = serde_json::from_value(value).expect("parse");
        assert_eq!(parsed.data_quality, doc.data_quality);
    }

    #[test]
    fn totals_cost_is_sum_of_model_row_costs() {
        // Feature D invariant: the global cost is the sum of the per-row
        // costs — each model prices its own tokens, so no other aggregation
        // is correct.
        let doc = seeded_doc();
        let sum: f64 = doc.model_usage.iter().map(|m| m.cost_usd).sum();
        assert!(sum > 0.0, "non-vacuous: seeded rows carry cost");
        assert!((doc.totals.cost_usd - sum).abs() < 1e-9);
    }

    #[test]
    fn model_row_cost_matches_pricing_from_parts() {
        // The seeded row is codex/gpt-5.5 with 700 in / 300 out / 120
        // cache-read: 700*5/1e6 + 300*30/1e6 + 120*0.5/1e6 = 0.01256
        // (meta() carries no pricing overrides).
        let doc = seeded_doc();
        let m = &doc.model_usage[0];
        assert_eq!(m.model, "gpt-5.5");
        let expected = crate::pricing::cost_from_parts(
            &m.group,
            &m.model,
            m.tokens_in,
            m.tokens_out,
            m.cache_read,
            m.cache_creation,
            &HashMap::new(),
        );
        assert!((m.cost_usd - expected).abs() < 1e-12);
        assert!((m.cost_usd - 0.012_56).abs() < 1e-9, "got {}", m.cost_usd);
    }

    #[test]
    fn doc_round_trips_through_json() {
        let doc = seeded_doc();
        let json = serde_json::to_string(&doc).expect("serialize");
        let parsed: DashboardDoc = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed.accounts.len(), doc.accounts.len());
        assert_eq!(parsed.accounts[0].name, "a");
        assert_eq!(
            parsed.accounts[0]
                .five_hour
                .as_ref()
                .expect("window")
                .fetched_at_ms,
            doc.accounts[0]
                .five_hour
                .as_ref()
                .expect("window")
                .fetched_at_ms
        );
        assert_eq!(
            parsed.activity.completed.len(),
            doc.activity.completed.len()
        );
        assert_eq!(parsed.model_usage.len(), doc.model_usage.len());
        assert_eq!(parsed.model_usage[0].model, "gpt-5.5");
        assert_eq!(parsed.model_usage[0].in_flight, 1);
        assert_eq!(parsed.logs[0].level, "INFO");
        // The JSON keys stay status-compatible ("type", not "kind").
        let value: serde_json::Value = serde_json::from_str(&json).expect("value");
        assert_eq!(value["accounts"][0]["type"], "oauth");
        assert!(value["accounts"][0]["five_hour"]["resets_in_secs"].is_u64());
    }

    #[test]
    fn activity_tail_caps_at_capacity() {
        let hub = DashboardHub::default();
        let seeded = ACTIVITY_TAIL as u64 + 30;
        for i in 0..seeded {
            hub.apply_event(
                ActivityEvent::RequestFinished {
                    id: i,
                    method: "POST".into(),
                    path: format!("/v1/messages/{i}"),
                    account: Some("a".into()),
                    status: 200,
                    duration: Duration::from_millis(10),
                    tokens: None,
                    group: None,
                    model: None,
                    effort: None,
                    fast: Some(false),
                    ttfb_ms: None,
                    ttft_ms: None,
                    gen_ms: None,
                    aborted: false,
                    user_id: None,
                    kind: None,
                    excerpt: None,
                    tenant: None,
                },
                now() - Duration::from_secs(seeded - i),
            );
        }
        let view = hub.view(now());
        assert_eq!(view.completed.len(), ACTIVITY_TAIL);
        // Newest first: the last-applied id leads.
        let newest = seeded - 1;
        match &view.completed[0].body {
            CompletedBody::Request { path, .. } => {
                assert_eq!(path, &format!("/v1/messages/{newest}"))
            }
            other => panic!("expected request, got {other:?}"),
        }
    }

    /// Throwaway on-disk activity log for the hydration tests.
    fn hydrate_tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "llmux-hub-hydrate-{}-{}-{tag}.jsonl",
            std::process::id(),
            ulid::Ulid::new()
        ))
    }

    fn finished_for(id: u64, account: &str) -> ActivityEvent {
        ActivityEvent::RequestFinished {
            id,
            method: "POST".into(),
            path: "/v1/messages".into(),
            account: Some(account.into()),
            status: 200,
            duration: Duration::from_millis(10),
            tokens: Some(TokenCounts {
                input: 10,
                output: 5,
                cache_read: None,
                cache_creation: None,
            }),
            group: Some("claude".into()),
            model: Some("sonnet".into()),
            effort: None,
            fast: Some(false),
            ttfb_ms: None,
            ttft_ms: None,
            gen_ms: None,
            aborted: false,
            user_id: None,
            kind: None,
            excerpt: None,
            tenant: None,
        }
    }

    /// The full lazy-hydration sequence a real serve runs: arm (cut captured),
    /// live traffic folds AND appends past the cut, then background hydration
    /// merges history behind it — every request counted exactly once, live
    /// rows in front, completion note on top.
    #[test]
    fn hydrate_persisted_merges_history_behind_live_without_double_count() {
        let path = hydrate_tmp("merge");
        // Two pre-boot historical records on disk.
        for (id, ts) in [(1, 100), (2, 200)] {
            crate::tui::activity::persist_request(
                Some(&path),
                &finished_for(id, "hist"),
                SystemTime::UNIX_EPOCH + Duration::from_secs(ts),
            );
        }

        let hub = DashboardHub::default();
        let cut = hub.arm_persistence(Some(path.clone()));
        assert!(cut > 0, "cut = pre-boot file length");
        // Live request while history is "still loading": folds into the hub
        // AND appends to the same file past the cut.
        hub.apply_event(finished_for(3, "live"), now());

        hub.hydrate_persisted(Some(&path), cut);

        let view = hub.view(now());
        assert_eq!(
            view.global_totals.requests, 3,
            "2 history + 1 live; the live append past the cut is not re-replayed"
        );
        assert_eq!(view.account_totals.get("hist").map(|t| t.requests), Some(2));
        assert_eq!(view.account_totals.get("live").map(|t| t.requests), Some(1));
        // Ring: completion note on top (it is the newest entry), then the live
        // request, then history behind it.
        match &view.completed[0].body {
            CompletedBody::Note { text, error } => {
                assert!(
                    text.contains("history loaded: 2"),
                    "completion note names the merged count: {text}"
                );
                assert!(!error);
            }
            other => panic!("expected the hydration note first, got {other:?}"),
        }
        let accounts: Vec<Option<String>> = view.completed[1..]
            .iter()
            .map(|c| match &c.body {
                CompletedBody::Request { account, .. } => account.clone(),
                other => panic!("expected requests behind the note, got {other:?}"),
            })
            .collect();
        assert_eq!(
            accounts,
            vec![
                Some("live".into()),
                Some("hist".into()),
                Some("hist".into())
            ],
            "live row stays in front, history behind"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A failed history read degrades to an empty history + a visible warning
    /// note — never a crash, and live state is untouched.
    #[test]
    fn hydrate_persisted_read_failure_warns_and_keeps_live_state() {
        let path = hydrate_tmp("bad-parent");
        std::fs::write(&path, b"x").expect("seed blocker file");
        let bad = path.join("activity.jsonl"); // parent is a FILE → open fails

        let hub = DashboardHub::default();
        hub.apply_event(finished_for(1, "live"), now());
        hub.hydrate_persisted(Some(&bad), 999);

        let view = hub.view(now());
        assert_eq!(view.global_totals.requests, 1, "live state untouched");
        match &view.completed[0].body {
            CompletedBody::Note { text, error } => {
                assert!(
                    text.contains("history load failed"),
                    "warning note surfaced: {text}"
                );
                assert!(error, "the note is an error note");
            }
            other => panic!("expected a warning note, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    /// First boot (no file yet → cut 0) and a vanished file are silent no-ops.
    #[test]
    fn hydrate_persisted_missing_file_and_zero_cut_are_noops() {
        let missing = hydrate_tmp("missing"); // never created
        let hub = DashboardHub::default();
        assert_eq!(
            hub.arm_persistence(Some(missing.clone())),
            0,
            "missing file → cut 0"
        );
        hub.hydrate_persisted(Some(&missing), 0); // zero cut: skip
        hub.hydrate_persisted(Some(&missing), 42); // vanished file: Ok, empty
        hub.hydrate_persisted(None, 42); // disabled: no-op
        let view = hub.view(now());
        assert_eq!(view.global_totals.requests, 0);
        assert!(view.completed.is_empty(), "no notes, no rows");
    }
}
