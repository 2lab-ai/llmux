//! glance-triage: the render-only triage layer over already-in-memory state.
//!
//! Three pure pieces, all consumed by `ui.rs`:
//!
//! 1. [`health_verdict`] — the always-present header verdict: an
//!    `[OK]/[WARN]/[FAIL]` tag plus ONE named dominant condition and a `+N`
//!    overflow marker, computed from a rolling 5-minute window
//!    ([`HealthCounts`]), poller staleness, and account states. Dominance:
//!    poller-stale / auth-broken, then 429·5xx storm, then exhausted, then
//!    healthy.
//! 2. [`intervention_order`] — the accounts table sorted by what the operator
//!    should act on, NOT registration/scheduler-preference order. in-flight is
//!    deliberately NOT a sort key (it toggles per request and would destroy
//!    the row-position memory a glance table exists for).
//! 3. [`collapse_completed`] — folds runs of at least [`FOLD_MIN`] CONSECUTIVE
//!    completed-2xx entries with an identical (method, path, account, group,
//!    model) key into one counted row. Non-2xx, in-flight, notes and control
//!    events never fold; non-consecutive entries are never grouped (that would
//!    reorder history).
//!
//! No persistence, no config surface: thresholds are the named constants below.

use std::cmp::Reverse;
use std::time::{Duration, SystemTime};

use crate::scheduler::select::{self, IneligibleReason, SelectParams};
use crate::scheduler::{AccountSnapshot, PoolSnapshot};

use super::activity::{Completed, CompletedBody};
use super::view::DashboardView;

/// A storm needs a SUSTAINED count in the 5m window — a single 429 is normal
/// backoff and must never flip the verdict (MUST-FIX 3).
pub(crate) const STORM_MIN_EVENTS: u64 = 10;
/// Generic error storm: at least this many errors AND at least half of the
/// window's requests failing.
pub(crate) const ERROR_STORM_MIN: u64 = 10;
/// An oauth poller whose last success is older than this is stale — the proxy
/// is flying blind on that account's quota.
pub(crate) const POLLER_STALE_AFTER: Duration = Duration::from_secs(300);
/// Or: this many consecutive poll failures, whichever trips first.
pub(crate) const POLLER_FAILS_MIN: u32 = 3;
/// Fold runs only from this length: 1–2 repeats read fine as-is.
pub(crate) const FOLD_MIN: usize = 3;

// ---------------------------------------------------------------------------
// Header verdict
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerdictLevel {
    Ok,
    Warn,
    Fail,
}

/// One named condition, worst-first. `account` carries the RAW id — the render
/// site masks it (`email_anonymous`) exactly like every other surface.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Condition {
    pub level: VerdictLevel,
    /// Condition name + numbers, without the account id ("429 STORM ×186/5m").
    pub text: String,
    /// Raw account id to append (masked at render), when account-scoped.
    pub account: Option<String>,
}

/// The header verdict: the single worst live condition plus how many more are
/// active. `conditions` is the full dominance-ordered list (first = headline).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Verdict {
    pub conditions: Vec<Condition>,
}

impl Verdict {
    pub(crate) fn level(&self) -> VerdictLevel {
        self.conditions
            .first()
            .map_or(VerdictLevel::Ok, |c| c.level)
    }

    pub(crate) fn headline(&self) -> Option<&Condition> {
        self.conditions.first()
    }

    /// Additional active conditions beyond the headline (the `+N` marker).
    pub(crate) fn more(&self) -> usize {
        self.conditions.len().saturating_sub(1)
    }
}

/// Compute the dominance-ordered condition list for one frame. Pure: reads
/// only the view + `now`, so a deterministic clock tests every threshold.
pub(crate) fn health_verdict(view: &DashboardView, now: SystemTime) -> Verdict {
    let mut conditions: Vec<Condition> = Vec::new();
    let snapshot = &view.snapshot;
    let params = &view.select_params;
    let headers_only = select::headers_only_mode(snapshot, params, None, now);

    // 1. Poller stale / auth broken — the proxy is BLIND, worse than a storm.
    let mut stale_poll: Option<(String, Duration)> = None;
    for account in &snapshot.accounts {
        if let Some(health) = view.poll_health(&account.id.0) {
            let aged = health
                .last_ok
                .and_then(|ok| now.duration_since(ok).ok())
                .filter(|&age| age > POLLER_STALE_AFTER);
            if health.consecutive_failures >= POLLER_FAILS_MIN || aged.is_some() {
                let age = aged.unwrap_or_default();
                // Keep the WORST (oldest) stale account as the representative.
                if stale_poll.as_ref().is_none_or(|(_, worst)| age > *worst) {
                    stale_poll = Some((account.id.0.clone(), age));
                }
            }
        }
    }
    if let Some((account, age)) = stale_poll {
        let since = if age > Duration::ZERO {
            format!(" last ok {}", select::compact_duration(age))
        } else {
            String::new()
        };
        conditions.push(Condition {
            level: VerdictLevel::Fail,
            text: format!("POLLER STALE{since}"),
            account: Some(account),
        });
    }
    let broken: Vec<&AccountSnapshot> = snapshot.accounts.iter().filter(|a| !a.healthy).collect();
    if let Some(first) = broken.first() {
        let extra = broken.len() - 1;
        let suffix = if extra > 0 {
            format!(" +{extra}")
        } else {
            String::new()
        };
        conditions.push(Condition {
            level: VerdictLevel::Fail,
            text: format!("AUTH BROKEN{suffix}"),
            account: Some(first.id.0.clone()),
        });
    }

    // 2. Storms over the rolling 5m window (dedicated per-second buckets,
    //    never the capacity-bounded ring — MUST-FIX 3). An old daemon sends
    //    no health telemetry (`None`): storm detection is UNAVAILABLE then,
    //    not "0 errors" — the header renders the err surface as `—`.
    if let Some(health) = view.health {
        if health.s429 >= STORM_MIN_EVENTS {
            conditions.push(Condition {
                level: VerdictLevel::Fail,
                text: format!("429 STORM ×{}/5m", health.s429),
                account: None,
            });
        } else if health.s5xx >= STORM_MIN_EVENTS {
            conditions.push(Condition {
                level: VerdictLevel::Fail,
                text: format!("5xx STORM ×{}/5m", health.s5xx),
                account: None,
            });
        } else if health.errors >= ERROR_STORM_MIN && health.errors * 2 >= health.requests {
            conditions.push(Condition {
                level: VerdictLevel::Fail,
                text: format!("ERROR STORM ×{}/5m", health.errors),
                account: None,
            });
        }
    }

    // 3. Exhausted / quota-critical accounts (worst utilization first).
    let mut exhausted: Vec<(&AccountSnapshot, &'static str, f64)> = snapshot
        .accounts
        .iter()
        .filter_map(|account| {
            let gate = select::eligibility(account, params, now, headers_only);
            let window = match gate {
                Some(IneligibleReason::FiveHourOverThreshold) => {
                    ("5h", account.five_hour.as_ref().map(|w| w.utilization))
                }
                Some(IneligibleReason::SevenDayOverThreshold) => {
                    ("7d", account.seven_day.as_ref().map(|w| w.utilization))
                }
                Some(IneligibleReason::FableWeeklyExhausted) => ("fable", None),
                _ => return None,
            };
            Some((account, window.0, window.1.unwrap_or(1.0)))
        })
        .collect();
    exhausted.sort_by(|a, b| b.2.total_cmp(&a.2));
    if let Some(&(account, window, util)) = exhausted.first() {
        let extra = exhausted.len() - 1;
        let suffix = if extra > 0 {
            format!(" +{extra}")
        } else {
            String::new()
        };
        conditions.push(Condition {
            level: VerdictLevel::Warn,
            text: format!("QUOTA CRITICAL {window} {:.0}%{suffix}", util * 100.0),
            account: Some(account.id.0.clone()),
        });
    }

    // 4. No health telemetry (attach to an old daemon): the verdict CANNOT
    //    say healthy — absence of data is never evidence of health. Lowest
    //    dominance: any real condition above still headlines, but a
    //    condition-free old daemon renders [WARN], not a false [OK].
    if view.health.is_none() {
        conditions.push(Condition {
            level: VerdictLevel::Warn,
            text: "ERR TELEMETRY UNAVAILABLE (old daemon)".to_string(),
            account: None,
        });
    }

    Verdict { conditions }
}

// ---------------------------------------------------------------------------
// Accounts intervention order
// ---------------------------------------------------------------------------

/// Urgency tier of one account row: lower renders higher. in-flight is NOT a
/// key anywhere here (MUST-FIX 4 — row jitter would destroy position memory).
fn tier(account: &AccountSnapshot, gate: Option<IneligibleReason>) -> u8 {
    match gate {
        Some(
            IneligibleReason::FiveHourOverThreshold
            | IneligibleReason::SevenDayOverThreshold
            | IneligibleReason::FableWeeklyExhausted,
        ) => 0,
        Some(IneligibleReason::AuthUnhealthy) => 1,
        // Cooling down still has KNOWN usage — it sorts with the usage tier.
        Some(IneligibleReason::CoolingDown | IneligibleReason::FableCoolingDown) => 2,
        None if account.five_hour.is_some() => 2,
        // Eligible but no 5h sample yet: "ready" — below the known rows.
        None => 3,
        Some(IneligibleReason::Paused) => 4,
        // cold / unknown last — distinctly BELOW paused so a stale account
        // can't shadow an operator decision. (#33 keeps the label distinct.)
        Some(IneligibleReason::UsageStale) => 5,
    }
}

/// Whether the row deserves the leading `!` urgency marker (tiers 0–1).
pub(crate) fn urgent(account: &AccountSnapshot, gate: Option<IneligibleReason>) -> bool {
    tier(account, gate) <= 1
}

/// Indices into `snapshot.accounts`, exhausted → auth-broken → known 5h desc
/// (cooldowns included) → ready → paused → cold/unknown; stable (config
/// index) within a tier so rows never swap without a state change.
pub(crate) fn intervention_order(
    snapshot: &PoolSnapshot,
    params: &SelectParams,
    now: SystemTime,
) -> Vec<usize> {
    let headers_only = select::headers_only_mode(snapshot, params, None, now);
    let mut order: Vec<usize> = (0..snapshot.accounts.len()).collect();
    order.sort_by_key(|&idx| {
        let account = &snapshot.accounts[idx];
        let gate = select::eligibility(account, params, now, headers_only);
        let tier = tier(account, gate);
        // Within the usage tier, higher 5h utilization = closer to the top
        // (permille avoids float keys). Other tiers keep config order.
        let usage_rank = if tier == 2 {
            let permille = account
                .five_hour
                .as_ref()
                .map(|w| (w.utilization.clamp(0.0, 1.0) * 1000.0) as u32)
                .unwrap_or(0);
            Reverse(permille)
        } else {
            Reverse(0)
        };
        (tier, usage_rank, idx)
    });
    order
}

// ---------------------------------------------------------------------------
// Activity run folding
// ---------------------------------------------------------------------------

/// One renderable activity row after folding: either a single entry or a run
/// of ≥[`FOLD_MIN`] consecutive same-key completed-2xx entries. Indices point
/// into the newest-first `completed` slice; a run's `start` is its NEWEST
/// entry. The run's stable CLICK identity is its OLDEST member — the newest
/// end grows with fresh traffic, the oldest survives until the ring drops it,
/// so an expanded run stays expanded across refreshes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActivityRow {
    Single(usize),
    Run { start: usize, len: usize },
}

/// The fold identity — (method, path, account, group, model): consecutive
/// completed entries with 2xx status and this exact key collapse into one
/// counted row.
type FoldKey<'a> = (
    &'a str,
    &'a str,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
);

/// `None` = never foldable. Only `count` probes group (Z 2026-07-15 "그루핑
/// count빼고 하지마"): count_tokens is the one traffic class Claude Code fires
/// in walls; every other kind — user turns, security passes, upstream errors —
/// renders 1:1 so nothing meaningful hides inside a fold. Notes, non-2xx and
/// keyless entries stay unfoldable as before.
fn fold_key(entry: &Completed) -> Option<FoldKey<'_>> {
    match &entry.body {
        CompletedBody::Request {
            method,
            path,
            account,
            status,
            group,
            model,
            kind,
            ..
        } if (200..300).contains(status) && kind.as_deref() == Some("count") => Some((
            method.as_str(),
            path.as_str(),
            account.as_deref(),
            group.as_deref(),
            model.as_deref(),
        )),
        _ => None,
    }
}

/// Whether a folded run is the one the operator expanded, and the key a
/// click should toggle. The expansion key is matched against EVERY member —
/// not just the oldest — so a long-lived run at the FULL ring's tail (whose
/// oldest member is evicted on each append) stays expanded until the clicked
/// member itself ages out of the ring. Returns the toggle key: the matched
/// member's key while expanded (so the next click collapses), else the
/// oldest member's key (the stable expand target).
pub(crate) fn run_toggle_key(
    run: &[Completed],
    expanded: Option<&super::activity::ActivityKey>,
) -> (bool, Option<super::activity::ActivityKey>) {
    if let Some(expanded) = expanded {
        for entry in run {
            if entry.activity_key().as_ref() == Some(expanded) {
                return (true, Some(expanded.clone()));
            }
        }
    }
    (false, run.last().and_then(|entry| entry.activity_key()))
}

/// Fold the newest-first completed list into render rows. Order-preserving:
/// only CONSECUTIVE entries group, so history is never rearranged.
pub(crate) fn collapse_completed(completed: &[Completed]) -> Vec<ActivityRow> {
    let mut rows: Vec<ActivityRow> = Vec::with_capacity(completed.len());
    let mut i = 0;
    while i < completed.len() {
        let Some(key) = fold_key(&completed[i]) else {
            rows.push(ActivityRow::Single(i));
            i += 1;
            continue;
        };
        let mut len = 1;
        while i + len < completed.len() && fold_key(&completed[i + len]) == Some(key) {
            len += 1;
        }
        if len >= FOLD_MIN {
            rows.push(ActivityRow::Run { start: i, len });
        } else {
            for offset in 0..len {
                rows.push(ActivityRow::Single(i + offset));
            }
        }
        i += len;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccountLimits;
    use crate::routing::BackendGroup;
    use crate::scheduler::window::{QuotaWindow, WindowSource};
    use crate::scheduler::AccountId;
    use crate::tui::activity::HealthCounts;
    use std::collections::BTreeMap;
    use std::time::UNIX_EPOCH;

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_000_000)
    }

    fn window(utilization: f64) -> QuotaWindow {
        QuotaWindow {
            utilization,
            resets_at: now() + Duration::from_secs(3600),
            fetched_at: now(),
            source: WindowSource::Headers,
        }
    }

    fn account(id: &str) -> AccountSnapshot {
        AccountSnapshot {
            id: AccountId(id.to_string()),
            healthy: true,
            credential_kind: "oauth",
            group: BackendGroup::Claude,
            five_hour: Some(window(0.10)),
            seven_day: Some(window(0.10)),
            scoped_limits: Vec::new(),
            scoped_cooldowns: Vec::new(),
            cooldown_until: None,
            cooldown_source: None,
            in_flight: 0,
            token_expires_at_ms: None,
            last_refresh_ms: None,
            paused: false,
            limits: AccountLimits::default(),
        }
    }

    fn pool(accounts: Vec<AccountSnapshot>) -> PoolSnapshot {
        PoolSnapshot {
            accounts,
            current: BTreeMap::new(),
            fable_current: BTreeMap::new(),
            manual_pin: Default::default(),
        }
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

    fn ordered_ids(snapshot: &PoolSnapshot) -> Vec<String> {
        intervention_order(snapshot, &params(), now())
            .into_iter()
            .map(|i| snapshot.accounts[i].id.0.clone())
            .collect()
    }

    // ---- intervention order ----

    #[test]
    fn exhausted_pins_top_then_auth_then_usage_desc() {
        let mut low = account("low");
        low.five_hour = Some(window(0.08));
        let mut high = account("high");
        high.five_hour = Some(window(0.30));
        let mut broken = account("broken");
        broken.healthy = false;
        let mut exhausted = account("exhausted");
        exhausted.five_hour = Some(window(0.97));
        // Registration order buries the urgent rows at the END on purpose.
        let snapshot = pool(vec![low, high, broken, exhausted]);
        assert_eq!(
            ordered_ids(&snapshot),
            vec!["exhausted", "broken", "high", "low"]
        );
    }

    #[test]
    fn in_flight_is_not_a_sort_key() {
        let mut idle_high = account("idle-high");
        idle_high.five_hour = Some(window(0.50));
        let mut busy_low = account("busy-low");
        busy_low.five_hour = Some(window(0.10));
        busy_low.in_flight = 3;
        let snapshot = pool(vec![idle_high, busy_low]);
        // Higher usage outranks in-flight activity; toggling in_flight can
        // never reorder rows.
        assert_eq!(ordered_ids(&snapshot), vec!["idle-high", "busy-low"]);
    }

    #[test]
    fn ties_are_stable_by_config_index() {
        let a = account("first");
        let b = account("second");
        let snapshot = pool(vec![a, b]);
        assert_eq!(ordered_ids(&snapshot), vec!["first", "second"]);
    }

    #[test]
    fn cold_unknown_sorts_below_ready_and_paused() {
        // cold = an oauth account whose usage sample went STALE (UsageStale
        // gate) — distinct from "ready" (eligible, just no 5h sample yet).
        let mut cold = account("cold");
        cold.five_hour = Some(QuotaWindow {
            utilization: 0.10,
            resets_at: now() + Duration::from_secs(3600),
            fetched_at: now() - Duration::from_secs(700), // > usage_max_age 600
            source: WindowSource::UsagePoll,
        });
        cold.seven_day = None;
        let ready = {
            let mut a = account("ready");
            a.five_hour = None;
            a.seven_day = None;
            a.credential_kind = "apikey";
            a
        };
        let mut paused = account("paused");
        paused.paused = true;
        let known = account("known");
        let snapshot = pool(vec![cold, ready, paused, known]);
        assert_eq!(
            ordered_ids(&snapshot),
            vec!["known", "ready", "paused", "cold"],
            "known usage > ready > paused > cold/stale, each its own tier"
        );
    }

    // ---- verdict ----

    #[test]
    fn old_daemon_without_health_never_claims_a_storm_verdict() {
        // `None` health = no telemetry (old daemon): storm detection is
        // unavailable, and account/poller conditions still work.
        let mut exhausted = account("exhausted");
        exhausted.five_hour = Some(window(0.97));
        let verdict = health_verdict(&view_without_health(pool(vec![exhausted])), now());
        assert_eq!(verdict.level(), VerdictLevel::Warn);
        assert!(verdict
            .headline()
            .expect("condition")
            .text
            .contains("QUOTA CRITICAL"));
    }

    fn view_with(snapshot: PoolSnapshot, health: HealthCounts) -> DashboardView {
        DashboardView {
            session_labels: Default::default(),
            grok: Default::default(),
            daily_usage: Vec::new(),
            usage_stats: Vec::new(),
            version: "llmux test".into(),
            pid: 1,
            uptime: Duration::from_secs(1),
            port: 3456,
            upstream: None,
            config_path: None,
            select_params: params(),
            refresh_ahead: Duration::from_secs(0),
            evaluate_tick: Duration::from_secs(60),
            snapshot,
            last_switch: None,
            poll_health: std::collections::HashMap::new(),
            session_totals: std::collections::HashMap::new(),
            global_totals: Default::default(),
            rpm_5m: 0.0,
            in_flight: Vec::new(),
            completed: Vec::new(),
            logs: Vec::new(),
            model_usage: Vec::new(),
            client_usage: Vec::new(),
            windowed: Vec::new(),
            codex: Default::default(),
            email_anonymous: false,
            tui_effects: true,
            gradient: crate::tui::ui::GradientCfg::default(),
            show_fable_weekly: false,
            domain_abbrev: BTreeMap::new(),
            quota_display: Default::default(),
            data_quality: Default::default(),
            events: Vec::new(),
            health: Some(health),
        }
    }

    fn view_without_health(snapshot: PoolSnapshot) -> DashboardView {
        let mut view = view_with(snapshot, HealthCounts::default());
        view.health = None;
        view
    }

    #[test]
    fn healthy_is_quiet_and_single_429_never_storms() {
        let health = HealthCounts {
            requests: 40,
            errors: 1,
            s429: 1,
            ..Default::default()
        };
        let verdict = health_verdict(&view_with(pool(vec![account("a")]), health), now());
        assert_eq!(verdict.level(), VerdictLevel::Ok);
        assert!(verdict.conditions.is_empty());
    }

    #[test]
    fn sustained_429s_trip_the_storm_threshold() {
        let health = HealthCounts {
            requests: 40,
            errors: STORM_MIN_EVENTS,
            s429: STORM_MIN_EVENTS,
            ..Default::default()
        };
        let verdict = health_verdict(&view_with(pool(vec![account("a")]), health), now());
        assert_eq!(verdict.level(), VerdictLevel::Fail);
        let head = verdict.headline().expect("condition");
        assert!(head.text.contains("429 STORM"), "got {}", head.text);
    }

    #[test]
    fn auth_broken_dominates_a_storm() {
        let mut broken = account("broken");
        broken.healthy = false;
        let health = HealthCounts {
            requests: 100,
            errors: 50,
            s429: 50,
            ..Default::default()
        };
        let verdict = health_verdict(&view_with(pool(vec![broken]), health), now());
        let head = verdict.headline().expect("condition");
        assert!(head.text.contains("AUTH BROKEN"), "got {}", head.text);
        assert_eq!(verdict.more(), 1, "storm stays visible as +1");
    }

    #[test]
    fn exhausted_account_is_a_warning_not_a_failure() {
        let mut exhausted = account("exhausted");
        exhausted.five_hour = Some(window(0.97));
        let verdict = health_verdict(
            &view_with(pool(vec![exhausted]), HealthCounts::default()),
            now(),
        );
        assert_eq!(verdict.level(), VerdictLevel::Warn);
        let head = verdict.headline().expect("condition");
        assert!(head.text.contains("QUOTA CRITICAL"), "got {}", head.text);
        assert_eq!(head.account.as_deref(), Some("exhausted"));
    }

    #[test]
    fn old_daemon_with_no_conditions_never_says_healthy() {
        // No account/poller condition AND no telemetry: the dangerous case —
        // the verdict must be a named WARN, not a false [OK] healthy.
        let verdict = health_verdict(&view_without_health(pool(vec![account("a")])), now());
        assert_eq!(verdict.level(), VerdictLevel::Warn);
        assert!(verdict
            .headline()
            .expect("condition")
            .text
            .contains("TELEMETRY UNAVAILABLE"));
    }

    // ---- activity folding ----

    fn request_kind(status: u16, path: &str, at_secs: u64, kind: Option<&str>) -> Completed {
        Completed {
            at: UNIX_EPOCH + Duration::from_secs(at_secs),
            body: CompletedBody::Request {
                id: 1,
                method: "POST".into(),
                path: path.into(),
                account: Some("a@x".into()),
                status,
                duration: Duration::from_secs(1),
                tokens: None,
                group: Some("claude".into()),
                model: Some("opus".into()),
                effort: None,
                fast: Some(false),
                ttfb_ms: None,
                ttft_ms: None,
                user_id: None,
                kind: kind.map(str::to_string),
                excerpt: None,
            },
        }
    }

    /// A foldable `count` probe — the only kind that groups (Z 2026-07-15).
    fn request(status: u16, path: &str, at_secs: u64) -> Completed {
        request_kind(status, path, at_secs, Some("count"))
    }

    fn note(at_secs: u64) -> Completed {
        Completed {
            at: UNIX_EPOCH + Duration::from_secs(at_secs),
            body: CompletedBody::Note {
                text: "switch a → b".into(),
                error: false,
            },
        }
    }

    #[test]
    fn consecutive_2xx_runs_fold_from_fold_min() {
        let entries = vec![
            request(200, "/v1/messages", 30),
            request(200, "/v1/messages", 20),
            request(200, "/v1/messages", 10),
        ];
        assert_eq!(
            collapse_completed(&entries),
            vec![ActivityRow::Run { start: 0, len: 3 }]
        );
    }

    #[test]
    fn only_count_kind_folds(/* Z 2026-07-15 "그루핑 count빼고 하지마" */) {
        // Identical consecutive 2xx entries that are NOT count probes (user
        // turns, security passes, untagged rows) render 1:1 — never folded.
        for kind in [Some("user"), Some("security"), None] {
            let entries = vec![
                request_kind(200, "/v1/messages", 30, kind),
                request_kind(200, "/v1/messages", 20, kind),
                request_kind(200, "/v1/messages", 10, kind),
            ];
            assert_eq!(
                collapse_completed(&entries),
                vec![
                    ActivityRow::Single(0),
                    ActivityRow::Single(1),
                    ActivityRow::Single(2),
                ],
                "kind {kind:?} must not fold"
            );
        }
    }

    #[test]
    fn short_runs_stay_single() {
        let entries = vec![
            request(200, "/v1/messages", 20),
            request(200, "/v1/messages", 10),
        ];
        assert_eq!(
            collapse_completed(&entries),
            vec![ActivityRow::Single(0), ActivityRow::Single(1)]
        );
    }

    #[test]
    fn non_2xx_and_notes_never_fold_and_break_runs() {
        let entries = vec![
            request(200, "/v1/messages", 60),
            request(200, "/v1/messages", 50),
            request(429, "/v1/messages", 40),
            request(429, "/v1/messages", 35),
            request(429, "/v1/messages", 33),
            request(200, "/v1/messages", 30),
            note(25),
            request(200, "/v1/messages", 20),
            request(200, "/v1/messages", 15),
            request(200, "/v1/messages", 10),
        ];
        let rows = collapse_completed(&entries);
        // 2×200 stay single; 3×429 stay single (errors NEVER fold); one 200
        // then a note then a foldable 3-run.
        assert_eq!(
            rows,
            vec![
                ActivityRow::Single(0),
                ActivityRow::Single(1),
                ActivityRow::Single(2),
                ActivityRow::Single(3),
                ActivityRow::Single(4),
                ActivityRow::Single(5),
                ActivityRow::Single(6),
                ActivityRow::Run { start: 7, len: 3 },
            ]
        );
    }

    #[test]
    fn run_expansion_survives_oldest_member_eviction() {
        // A full ring evicts its overall-oldest entry on every append. The
        // expansion must match ANY member, so the run the operator expanded
        // stays open when its (previous) oldest member is evicted — and the
        // toggle key echoes the matched member so the next click collapses.
        let run = vec![
            request(200, "/v1/messages", 40),
            request(200, "/v1/messages", 30),
            request(200, "/v1/messages", 20),
        ];
        let clicked = run[1].activity_key().expect("key");
        let (expanded, toggle) = run_toggle_key(&run, Some(&clicked));
        assert!(expanded);
        assert_eq!(toggle.as_ref(), Some(&clicked));
        // Once the clicked member itself ages out, the run collapses
        // gracefully and re-arms on the new oldest member.
        let evicted = vec![run[0].clone(), run[1].clone()];
        let old_oldest = request(200, "/v1/messages", 10)
            .activity_key()
            .expect("key");
        let (expanded, toggle) = run_toggle_key(&evicted, Some(&old_oldest));
        assert!(!expanded);
        assert_eq!(toggle, evicted[1].activity_key());
    }

    #[test]
    fn key_change_breaks_a_run() {
        let entries = vec![
            request(200, "/v1/messages", 40),
            request(200, "/v1/messages", 30),
            request(200, "/other", 20),
            request(200, "/v1/messages", 10),
        ];
        let rows = collapse_completed(&entries);
        assert!(rows.iter().all(|r| matches!(r, ActivityRow::Single(_))));
    }
}
