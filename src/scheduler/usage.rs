//! Active quota tracking: per-account `GET /api/oauth/usage` poller with a
//! backoff ladder, so idle accounts (which produce no headers) still have
//! fresh window state.
//!
//! The HTTP call is injectable (`UsageFetcher`) so the scheduling logic is
//! testable without a network; `ReqwestFetcher` is the production impl.

use std::collections::HashMap;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use super::headers::{parse_epoch_seconds, parse_rfc3339, WindowReading};
use super::window::LimitSeverity;
use super::{AccountId, AccountPool};
use crate::config::{AccountCredential, SchedulerConfig};

/// Parsed body of `GET /api/oauth/usage` (Bearer auth): per-window
/// utilization + resets_at, same shape soma-work polls every 5 minutes.
///
/// `scoped` carries the model-scoped rows of the body's `limits[]` array
/// (`kind == "weekly_scoped"`, e.g. the "Fable" weekly gauge) — empty when
/// the response has no `limits[]` (older shape) or no scoped rows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UsageSnapshot {
    pub five_hour: Option<WindowReading>,
    pub seven_day: Option<WindowReading>,
    pub scoped: Vec<ScopedLimitReading>,
}

/// One model-scoped limit reading from `limits[]`: the scope label
/// (`scope.model.display_name`, e.g. "Fable" — NOT hardcoded here; the list
/// is model-extensible), the percentage-normalized window reading, and the
/// row's `severity`/`is_active` flags.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedLimitReading {
    pub scope_label: String,
    pub reading: WindowReading,
    pub severity: LimitSeverity,
    pub is_active: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum UsageError {
    #[error("usage endpoint http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("usage endpoint returned {status}")]
    Status { status: http::StatusCode },
    #[error("usage body parse error: {0}")]
    Parse(#[from] serde_json::Error),
}

/// Parse the usage endpoint body. Tolerant by design: a missing window is
/// `None`, `resets_at` accepts epoch seconds or an RFC3339 string, and only
/// undecodable JSON is an error.
///
/// SCALE IS FIXED, NOT GUESSED. The live `GET /api/oauth/usage` endpoint
/// always returns `utilization` as a PERCENTAGE 0..=100 — exactly like the
/// codex `x-codex-*-used-percent` headers, and unlike the Anthropic unified
/// headers which are 0..=1 fractions. Each evidence source has its own known
/// scale; this one is divided by 100 unconditionally.
///
/// Ground truth (captured 2026-06-14, three live accounts):
///   ai3@: five_hour=5.0, seven_day=3.0      (== 5% / 3%)
///   ai@:  five_hour=1.0, seven_day=0.0      (== 1% / 0%)
///   ai2@: five_hour=1.0, seven_day=1.0      (== 1% / 1%)
///
/// The previous code guessed the scale per response (`as_percentage =
/// max_raw > 1.0`) and treated all-≤1.0 responses as fractions. That stranded
/// any account whose every window sat at ≤1% utilization: ai@/ai2@ above were
/// recorded as 1.0 == 100% and gated as exhausted while in fact ~1% used and
/// fully available. ai3@ only escaped because its 5.0 happened to exceed 1.0.
/// There is no fraction-form response from this endpoint to preserve.
pub fn parse_usage_body(body: &[u8]) -> Result<UsageSnapshot, UsageError> {
    let value: Value = serde_json::from_slice(body)?;
    Ok(UsageSnapshot {
        five_hour: raw_window(value.get("five_hour")).map(|(u, at)| percent_reading(u, at)),
        seven_day: raw_window(value.get("seven_day")).map(|(u, at)| percent_reading(u, at)),
        scoped: scoped_limits(&value),
    })
}

/// Extract the model-scoped rows of `limits[]` (evidence:
/// `.prd/13-usage-raw-sources.md` §Carrier 1, captured 2026-07-03).
///
/// Only `kind == "weekly_scoped"` rows become scoped readings, keyed by
/// `scope.model.display_name`. The `session`/`weekly_all` rows duplicate the
/// legacy top-level `five_hour`/`seven_day` fields (which stay the canonical
/// parse for those windows, and the fallback when `limits[]` is absent), so
/// they are deliberately skipped here. `percent` is a 0..=100 int and is
/// normalized to a 0..1 fraction like every other reading from this endpoint.
/// Tolerant by design: a missing/invalid row is dropped, never an error.
fn scoped_limits(value: &Value) -> Vec<ScopedLimitReading> {
    let Some(limits) = value.get("limits").and_then(Value::as_array) else {
        return Vec::new();
    };
    limits.iter().filter_map(scoped_limit_row).collect()
}

/// Parse one `limits[]` row into a scoped reading, or `None` for non-scoped
/// kinds and malformed rows.
fn scoped_limit_row(row: &Value) -> Option<ScopedLimitReading> {
    if row.get("kind")?.as_str()? != "weekly_scoped" {
        return None;
    }
    let label = row
        .get("scope")?
        .get("model")?
        .get("display_name")?
        .as_str()?;
    if label.is_empty() {
        return None;
    }
    let percent = row.get("percent")?.as_f64()?;
    if !percent.is_finite() || percent < 0.0 {
        return None;
    }
    let resets_at = match row.get("resets_at")? {
        Value::Number(n) => parse_epoch_seconds(&n.to_string())?,
        Value::String(s) => parse_rfc3339(s).or_else(|| parse_epoch_seconds(s))?,
        _ => return None,
    };
    let severity = row
        .get("severity")
        .and_then(Value::as_str)
        .map(LimitSeverity::from_label)
        .unwrap_or(LimitSeverity::Normal);
    let is_active = row
        .get("is_active")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(ScopedLimitReading {
        scope_label: label.to_string(),
        reading: percent_reading(percent, resets_at),
        severity,
        is_active,
    })
}

/// The VERBATIM `GET /api/oauth/usage` body captured live on
/// `claude:dev1@insightquest.io`, 2026-07-03 (`.prd/13-usage-raw-sources.md`
/// §Carrier 1) — the ground-truth fixture for `limits[]` parsing, shared with
/// the `/llmux/status` end-to-end test.
#[cfg(test)]
pub(crate) const DEV1_USAGE_FIXTURE: &str = r#"{
 "five_hour":  { "utilization": 0.0,  "resets_at": "2026-07-03T07:29:59.682460+00:00",
                 "limit_dollars": null, "used_dollars": null, "remaining_dollars": null },
 "seven_day":  { "utilization": 58.0, "resets_at": "2026-07-03T21:59:59.682491+00:00",
                 "limit_dollars": null, "used_dollars": null, "remaining_dollars": null },
 "seven_day_oauth_apps": null, "seven_day_opus": null, "seven_day_sonnet": null,
 "seven_day_cowork": null, "seven_day_omelette": null,
 "tangelo": null, "iguana_necktie": null, "omelette_promotional": null,
 "nimbus_quill": null, "cinder_cove": null, "amber_ladder": null,
 "extra_usage": { "is_enabled": false, "monthly_limit": null, "used_credits": null,
                  "utilization": null, "currency": null, "decimal_places": null,
                  "disabled_reason": null, "daily": null, "weekly": null },
 "limits": [
  { "kind": "session",       "group": "session", "percent": 0,   "severity": "normal",
    "resets_at": "2026-07-03T07:29:59.682460+00:00", "scope": null, "is_active": false },
  { "kind": "weekly_all",    "group": "weekly",  "percent": 58,  "severity": "normal",
    "resets_at": "2026-07-03T21:59:59.682491+00:00", "scope": null, "is_active": false },
  { "kind": "weekly_scoped", "group": "weekly",  "percent": 100, "severity": "critical",
    "resets_at": "2026-07-03T21:59:59.682835+00:00",
    "scope": { "model": { "id": null, "display_name": "Fable" }, "surface": null },
    "is_active": true }
 ],
 "spend": { "used": {"amount_minor": 0, "currency": "USD", "exponent": 2}, "limit": null,
            "percent": 0, "severity": "normal", "enabled": false, "disabled_reason": null,
            "cap": null, "balance": null, "auto_reload": null,
            "disclaimer": "Usage credits cover you when you hit your plan limits. …",
            "can_purchase_credits": false, "can_toggle": false },
 "member_dashboard_available": false
}"#;

/// Parse one window's RAW (still-percentage) utilization + reset, or `None`
/// when either is missing/invalid. The caller divides by 100 via
/// [`percent_reading`].
fn raw_window(value: Option<&Value>) -> Option<(f64, std::time::SystemTime)> {
    let value = value?;
    let raw = value.get("utilization")?.as_f64()?;
    if !raw.is_finite() || raw < 0.0 {
        return None;
    }
    let resets_at = match value.get("resets_at")? {
        Value::Number(n) => parse_epoch_seconds(&n.to_string())?,
        Value::String(s) => parse_rfc3339(s).or_else(|| parse_epoch_seconds(s))?,
        _ => return None,
    };
    Some((raw, resets_at))
}

/// Convert a percentage (0..=100) utilization to a clamped 0..1 fraction.
fn percent_reading(percent: f64, resets_at: std::time::SystemTime) -> WindowReading {
    WindowReading {
        utilization: (percent / 100.0).clamp(0.0, 1.0),
        resets_at,
    }
}

/// One-shot fetch of usage for one oauth account. Pure IO — no pool access —
/// so it is independently testable against the mock upstream.
pub async fn fetch_usage(
    client: &reqwest::Client,
    base_url: &str,
    access_token: &str,
) -> Result<UsageSnapshot, UsageError> {
    let url = format!("{}/api/oauth/usage", base_url.trim_end_matches('/'));
    let response = client
        .get(url)
        .bearer_auth(access_token)
        .header(http::header::ACCEPT, "application/json")
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(UsageError::Status { status });
    }
    let body = response.bytes().await?;
    parse_usage_body(&body)
}

/// Injectable transport for the usage endpoint, so the poller is testable
/// without a network.
pub trait UsageFetcher: Send + Sync {
    fn fetch(
        &self,
        base_url: &str,
        access_token: &str,
    ) -> impl Future<Output = Result<UsageSnapshot, UsageError>> + Send;
}

/// Production fetcher backed by `reqwest`.
#[derive(Clone)]
pub struct ReqwestFetcher {
    client: reqwest::Client,
}

impl UsageFetcher for ReqwestFetcher {
    fn fetch(
        &self,
        base_url: &str,
        access_token: &str,
    ) -> impl Future<Output = Result<UsageSnapshot, UsageError>> + Send {
        let client = self.client.clone();
        let base_url = base_url.to_owned();
        let access_token = access_token.to_owned();
        async move { fetch_usage(&client, &base_url, &access_token).await }
    }
}

/// Failure backoff ladder (task spec): 2m → 5m → 10m → 15m cap. Zero
/// failures means the regular poll interval.
const BACKOFF_LADDER_SECS: [u64; 4] = [120, 300, 600, 900];

/// Scheduling granularity of the poll loop.
const POLL_TICK: Duration = Duration::from_secs(5);

/// Minimum wall-clock gap between any two usage polls, across ALL accounts. The
/// poller fires at most one `/api/oauth/usage` call per gap, one account at a
/// time, so a tick that finds many accounts due (e.g. the priming tick at
/// startup, where every account's `next_at` is `now`) never bursts a call per
/// account. A burst across all accounts can trip the upstream's org/IP
/// request-rate limit and make llmux rate-limit its own traffic.
const MIN_POLL_GAP: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy)]
struct PollSchedule {
    next_at: SystemTime,
    consecutive_failures: u32,
}

/// Background poller: polls every oauth account at `usage_poll_secs` cadence
/// with jitter; failures climb the backoff ladder and recover on first
/// success. Each account has its own next-allowed-at; API-key accounts are
/// skipped (no usage endpoint).
pub struct UsagePoller<F = ReqwestFetcher> {
    pool: AccountPool,
    fetcher: F,
    base_url: String,
    config: SchedulerConfig,
    schedule: HashMap<AccountId, PollSchedule>,
    /// Wall-clock time of the last poll, for the global [`MIN_POLL_GAP`] throttle.
    last_poll_at: Option<SystemTime>,
    /// Best-effort poller-health feed to the dashboard (`try_send`, dropped
    /// on a full channel — same contract as every activity sender).
    events: Option<tokio::sync::mpsc::Sender<crate::tui::ActivityEvent>>,
}

impl UsagePoller<ReqwestFetcher> {
    pub fn new(
        pool: AccountPool,
        client: reqwest::Client,
        base_url: String,
        config: SchedulerConfig,
    ) -> Self {
        Self::with_fetcher(pool, ReqwestFetcher { client }, base_url, config)
    }
}

impl<F: UsageFetcher> UsagePoller<F> {
    /// Build a poller with a custom transport (tests inject a mock here).
    pub fn with_fetcher(
        pool: AccountPool,
        fetcher: F,
        base_url: String,
        config: SchedulerConfig,
    ) -> Self {
        Self {
            pool,
            fetcher,
            base_url,
            config,
            schedule: HashMap::new(),
            last_poll_at: None,
            events: None,
        }
    }

    /// Attach the activity-event sender; each poll attempt then emits a
    /// `UsagePolled` event for the dashboard's poller-health pane.
    pub fn with_events(
        mut self,
        events: Option<tokio::sync::mpsc::Sender<crate::tui::ActivityEvent>>,
    ) -> Self {
        self.events = events;
        self
    }

    /// Run forever (spawned as a background task next to the proxy server).
    pub async fn run(mut self) {
        loop {
            self.tick(SystemTime::now()).await;
            tokio::time::sleep(POLL_TICK).await;
        }
    }

    /// Re-read the oauth roster, drop schedules for removed accounts, and give
    /// every current account a schedule entry (new accounts due immediately).
    fn refresh_schedule(&mut self, now: SystemTime) -> Vec<AccountId> {
        let oauth_ids: Vec<AccountId> = self
            .pool
            .snapshot()
            .accounts
            .iter()
            .filter(|a| a.credential_kind == "oauth")
            .map(|a| a.id.clone())
            .collect();
        self.schedule.retain(|id, _| oauth_ids.contains(id));
        for id in &oauth_ids {
            self.schedule.entry(id.clone()).or_insert(PollSchedule {
                next_at: now,
                consecutive_failures: 0,
            });
        }
        oauth_ids
    }

    /// Poll one account and reschedule it (jittered interval on success, backoff
    /// ladder on failure), emitting a poller-health event.
    async fn poll_and_reschedule(&mut self, id: AccountId, now: SystemTime) {
        let prev_failures = self.schedule.get(&id).map_or(0, |e| e.consecutive_failures);
        let failures = match self.poll_account(&id, now).await {
            Ok(()) => 0,
            Err(err) => {
                tracing::warn!(account = %id, error = %err, "usage poll failed");
                prev_failures.saturating_add(1)
            }
        };
        let delay = jittered(self.backoff_delay(failures), &id, now);
        if let Some(events) = &self.events {
            let _ = events.try_send(crate::tui::ActivityEvent::UsagePolled {
                account: id.0.clone(),
                ok: failures == 0,
                consecutive_failures: failures,
                next_in: delay,
            });
        }
        self.schedule.insert(
            id,
            PollSchedule {
                next_at: now + delay,
                consecutive_failures: failures,
            },
        );
    }

    /// Startup priming: poll EVERY due account once so the first selection ranks
    /// on real window data. This is a one-time burst at boot; the ongoing
    /// [`Self::tick`] throttles to one poll per [`MIN_POLL_GAP`] so the poller
    /// never *continuously* bursts a call per account (which can trip the
    /// upstream's org/IP request-rate limit).
    pub async fn prime(&mut self, now: SystemTime) {
        for id in self.refresh_schedule(now) {
            if self.schedule.get(&id).is_some_and(|e| e.next_at <= now) {
                self.poll_and_reschedule(id, now).await;
            }
        }
        self.last_poll_at = Some(now);
    }

    /// One scheduling pass: poll AT MOST ONE due account (the most overdue),
    /// throttled to one poll per [`MIN_POLL_GAP`] across all accounts. Re-reads
    /// the roster each pass so account reloads are picked up; removed accounts
    /// drop their schedule entries.
    pub async fn tick(&mut self, now: SystemTime) {
        let oauth_ids = self.refresh_schedule(now);

        // Global throttle: at most one poll per MIN_POLL_GAP, so a pass that
        // finds many accounts due never bursts a call per account.
        if self
            .last_poll_at
            .is_some_and(|last| now.duration_since(last).is_ok_and(|gap| gap < MIN_POLL_GAP))
        {
            return;
        }

        // Poll the single most-overdue due account this tick.
        let Some(id) = oauth_ids
            .iter()
            .filter(|id| self.schedule.get(*id).is_some_and(|e| e.next_at <= now))
            .min_by_key(|id| self.schedule.get(*id).map(|e| e.next_at).unwrap_or(now))
            .cloned()
        else {
            return;
        };
        self.last_poll_at = Some(now);
        self.poll_and_reschedule(id, now).await;
    }

    /// Compute the next delay after `consecutive_failures` for one account —
    /// pure, unit-testable backoff ladder (2m → 5m → 10m → 15m cap; zero
    /// failures = the configured poll interval).
    pub fn backoff_delay(&self, consecutive_failures: u32) -> Duration {
        if consecutive_failures == 0 {
            return Duration::from_secs(self.config.usage_poll_secs);
        }
        let idx = (consecutive_failures as usize - 1).min(BACKOFF_LADDER_SECS.len() - 1);
        Duration::from_secs(BACKOFF_LADDER_SECS[idx])
    }

    /// Poll a single account once and record the outcome. Non-oauth (or
    /// vanished) accounts are a no-op. A 403 means the token was revoked —
    /// surfaced as an auth failure; a 401 is left for the auth layer's
    /// refresh path (the next poll retries with the refreshed credential).
    pub async fn poll_account(
        &self,
        account: &AccountId,
        now: SystemTime,
    ) -> Result<(), UsageError> {
        let Some(AccountCredential::Oauth { access_token, .. }) = self.pool.credential(account)
        else {
            return Ok(());
        };
        match self.fetcher.fetch(&self.base_url, &access_token).await {
            Ok(snapshot) => {
                self.pool.record_usage(account, &snapshot, now);
                Ok(())
            }
            Err(err) => {
                if let UsageError::Status { status } = &err {
                    if *status == http::StatusCode::FORBIDDEN {
                        self.pool.record_auth_failure(account);
                    }
                }
                Err(err)
            }
        }
    }
}

/// Deterministic-enough jitter: up to +10% of `base`, seeded from the
/// account id and the current tick. No rand dependency needed for spreading
/// poll times across accounts.
fn jittered(base: Duration, id: &AccountId, now: SystemTime) -> Duration {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    now.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos())
        .hash(&mut hasher);
    let fraction = (hasher.finish() % 1000) as f64 / 1000.0;
    base + base.mul_f64(0.1 * fraction)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccountConfig;
    use std::sync::Mutex;

    const NOW_SECS: u64 = 1_000_000;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn now() -> SystemTime {
        at(NOW_SECS)
    }

    fn id(s: &str) -> AccountId {
        AccountId(s.to_string())
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

    fn apikey_account(name: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            credential: AccountCredential::Apikey {
                api_key: format!("sk-ant-{name}"),
            },
        }
    }

    fn config() -> SchedulerConfig {
        SchedulerConfig::default() // poll 300s, max age 600s
    }

    /// Scripted fetcher: pops the next queued result per call and records
    /// the tokens it was called with.
    struct MockFetcher {
        results: Mutex<Vec<Result<UsageSnapshot, UsageError>>>,
        calls: Mutex<Vec<String>>,
    }

    impl MockFetcher {
        fn new(results: Vec<Result<UsageSnapshot, UsageError>>) -> Self {
            Self {
                results: Mutex::new(results),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl UsageFetcher for &MockFetcher {
        fn fetch(
            &self,
            _base_url: &str,
            access_token: &str,
        ) -> impl Future<Output = Result<UsageSnapshot, UsageError>> + Send {
            self.calls.lock().unwrap().push(access_token.to_string());
            let result = {
                let mut results = self.results.lock().unwrap();
                if results.is_empty() {
                    Ok(UsageSnapshot::default())
                } else {
                    results.remove(0)
                }
            };
            async move { result }
        }
    }

    fn status_err(code: u16) -> UsageError {
        UsageError::Status {
            status: http::StatusCode::from_u16(code).unwrap(),
        }
    }

    fn snapshot_with(util: f64) -> UsageSnapshot {
        UsageSnapshot {
            five_hour: Some(WindowReading {
                utilization: util,
                resets_at: at(NOW_SECS + 3600),
            }),
            seven_day: None,
            scoped: Vec::new(),
        }
    }

    // ---- body parsing ----

    #[test]
    fn parses_percentage_utilization_and_rfc3339_reset() {
        // Endpoint sends percentages (0..=100); parser divides by 100.
        let body = br#"{
            "five_hour": {"utilization": 42.0, "resets_at": "2026-06-12T00:00:00Z"},
            "seven_day": {"utilization": 90.0, "resets_at": "2026-06-14T00:00:00Z"}
        }"#;
        let snapshot = parse_usage_body(body).unwrap();
        assert!((snapshot.five_hour.unwrap().utilization - 0.42).abs() < 1e-9);
        assert_eq!(
            snapshot.five_hour.unwrap().resets_at,
            at(1_781_222_400) // 2026-06-12T00:00:00Z
        );
        assert!((snapshot.seven_day.unwrap().utilization - 0.90).abs() < 1e-9);
    }

    #[test]
    fn percentage_utilization_is_normalized() {
        let body = br#"{"five_hour": {"utilization": 42, "resets_at": 1781222400}}"#;
        let snapshot = parse_usage_body(body).unwrap();
        assert!((snapshot.five_hour.unwrap().utilization - 0.42).abs() < 1e-9);
    }

    #[test]
    fn sub_one_percent_seven_day_is_not_misread_as_full() {
        // Ground truth from the live endpoint (2026-06-13): percentages, with a
        // 7d of 1.0 meaning 1% — NOT the fraction 1.0 (100%). The 5h value
        // (16.0) sets the response scale, so the 7d normalizes to 0.01.
        let body = br#"{
            "five_hour": {"utilization": 16.0, "resets_at": 1781350800},
            "seven_day": {"utilization": 1.0, "resets_at": 1781946000}
        }"#;
        let snapshot = parse_usage_body(body).unwrap();
        assert!((snapshot.five_hour.unwrap().utilization - 0.16).abs() < 1e-9);
        assert!(
            (snapshot.seven_day.unwrap().utilization - 0.01).abs() < 1e-9,
            "7d at 1.0% must read as 0.01, not 1.0 (the old per-window bug)"
        );
    }

    #[test]
    fn low_utilization_accounts_are_not_misread_as_full() {
        // Regression for the live 2026-06-14 bug. Both accounts were ~1% used
        // and fully available, but the old per-response scale guess
        // (`max_raw > 1.0` ⇒ percentage, else fraction) read every window
        // whose values were all ≤ 1.0 as fractions and recorded 1.0 == 100%,
        // gating them as exhausted.

        // ai2@: five_hour=1.0, seven_day=1.0  (== 1% / 1%, NOT 100% / 100%)
        let ai2 = br#"{
            "five_hour": {"utilization": 1.0, "resets_at": 1781222400},
            "seven_day": {"utilization": 1.0, "resets_at": 1781222400}
        }"#;
        let s = parse_usage_body(ai2).unwrap();
        assert!((s.five_hour.unwrap().utilization - 0.01).abs() < 1e-9);
        assert!((s.seven_day.unwrap().utilization - 0.01).abs() < 1e-9);

        // ai@: five_hour=1.0, seven_day=0.0  (== 1% / 0%). A single sub-1.0
        // window must still be a percentage, not a fraction.
        let ai = br#"{
            "five_hour": {"utilization": 1.0, "resets_at": 1781222400},
            "seven_day": {"utilization": 0.0, "resets_at": 1781222400}
        }"#;
        let s = parse_usage_body(ai).unwrap();
        assert!((s.five_hour.unwrap().utilization - 0.01).abs() < 1e-9);
        assert_eq!(s.seven_day.unwrap().utilization, 0.0);
    }

    #[test]
    fn over_one_hundred_percent_clamps_to_full() {
        let body = br#"{"five_hour": {"utilization": 137.0, "resets_at": 1781222400}}"#;
        assert_eq!(
            parse_usage_body(body)
                .unwrap()
                .five_hour
                .unwrap()
                .utilization,
            1.0
        );
    }

    #[test]
    fn epoch_number_reset_is_accepted() {
        let body = br#"{"seven_day": {"utilization": 0.5, "resets_at": 1781222400}}"#;
        let snapshot = parse_usage_body(body).unwrap();
        assert_eq!(snapshot.seven_day.unwrap().resets_at, at(1_781_222_400));
        assert!(snapshot.five_hour.is_none());
    }

    #[test]
    fn missing_windows_are_none_not_errors() {
        let snapshot = parse_usage_body(b"{}").unwrap();
        assert_eq!(snapshot, UsageSnapshot::default());
    }

    #[test]
    fn malformed_window_fields_are_dropped() {
        let body = br#"{"five_hour": {"utilization": "high", "resets_at": true}}"#;
        assert!(parse_usage_body(body).unwrap().five_hour.is_none());
    }

    #[test]
    fn invalid_json_is_a_parse_error() {
        assert!(matches!(
            parse_usage_body(b"not json"),
            Err(UsageError::Parse(_))
        ));
    }

    // ---- limits[] scoped rows (fable-usage W1) ----

    #[test]
    fn dev1_fixture_yields_legacy_windows_and_a_fable_scoped_reading() {
        // The verbatim live capture (.prd/13-usage-raw-sources.md §Carrier 1):
        // legacy five_hour/seven_day parse exactly as before, AND the
        // limits[] weekly_scoped row becomes a "Fable" scoped reading at
        // 100% / critical / active. session + weekly_all rows are skipped
        // (they duplicate the legacy fields).
        let snapshot = parse_usage_body(DEV1_USAGE_FIXTURE.as_bytes()).unwrap();

        let five = snapshot.five_hour.unwrap();
        assert_eq!(five.utilization, 0.0);
        assert_eq!(epoch_of(five.resets_at), 1_783_063_799); // 2026-07-03T07:29:59Z
        let seven = snapshot.seven_day.unwrap();
        assert!((seven.utilization - 0.58).abs() < 1e-9);
        assert_eq!(epoch_of(seven.resets_at), 1_783_115_999); // 2026-07-03T21:59:59Z

        assert_eq!(snapshot.scoped.len(), 1, "only the weekly_scoped row");
        let fable = &snapshot.scoped[0];
        assert_eq!(fable.scope_label, "Fable");
        assert_eq!(fable.reading.utilization, 1.0, "percent 100 → fraction 1.0");
        assert_eq!(epoch_of(fable.reading.resets_at), 1_783_115_999);
        assert_eq!(fable.severity, LimitSeverity::Critical);
        assert!(fable.is_active);
    }

    fn epoch_of(at: SystemTime) -> u64 {
        at.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs()
    }

    #[test]
    fn legacy_only_response_has_empty_scoped_and_unchanged_legacy_windows() {
        // A response WITHOUT limits[] (older shape) must behave exactly as
        // before: legacy windows parse, scoped list is empty.
        let body = br#"{
            "five_hour": {"utilization": 42.0, "resets_at": 1781222400},
            "seven_day": {"utilization": 90.0, "resets_at": 1781308800}
        }"#;
        let snapshot = parse_usage_body(body).unwrap();
        assert!((snapshot.five_hour.unwrap().utilization - 0.42).abs() < 1e-9);
        assert!((snapshot.seven_day.unwrap().utilization - 0.90).abs() < 1e-9);
        assert!(snapshot.scoped.is_empty(), "no limits[] → no scoped rows");
    }

    #[test]
    fn malformed_scoped_rows_are_dropped_not_errors() {
        // Rows missing the scope label / percent / resets_at, non-array
        // limits, and unknown severities degrade gracefully.
        let body = br#"{
            "limits": [
                { "kind": "weekly_scoped", "percent": 50, "severity": "critical",
                  "resets_at": 1781222400, "scope": null, "is_active": true },
                { "kind": "weekly_scoped", "percent": 50,
                  "resets_at": 1781222400,
                  "scope": { "model": { "id": null, "display_name": "" } } },
                { "kind": "weekly_scoped", "resets_at": 1781222400,
                  "scope": { "model": { "display_name": "Fable" } } },
                { "kind": "weekly_scoped", "percent": 61, "severity": "sev-from-the-future",
                  "resets_at": 1781222400,
                  "scope": { "model": { "display_name": "Opus" } } }
            ]
        }"#;
        let snapshot = parse_usage_body(body).unwrap();
        assert_eq!(snapshot.scoped.len(), 1, "only the well-formed row");
        let opus = &snapshot.scoped[0];
        assert_eq!(opus.scope_label, "Opus");
        assert!((opus.reading.utilization - 0.61).abs() < 1e-9);
        assert_eq!(
            opus.severity,
            LimitSeverity::Normal,
            "unknown severity degrades to normal"
        );
        assert!(!opus.is_active, "missing is_active defaults to false");
    }

    #[test]
    fn non_array_limits_is_tolerated() {
        let body = br#"{"limits": {"kind": "weekly_scoped"}}"#;
        assert!(parse_usage_body(body).unwrap().scoped.is_empty());
    }

    // ---- backoff ladder ----

    #[test]
    fn backoff_ladder_matches_spec() {
        let pool = AccountPool::new(&[]);
        let fetcher = MockFetcher::new(vec![]);
        let poller = UsagePoller::with_fetcher(pool, &fetcher, "http://x".into(), config());
        assert_eq!(poller.backoff_delay(0), Duration::from_secs(300));
        assert_eq!(poller.backoff_delay(1), Duration::from_secs(120));
        assert_eq!(poller.backoff_delay(2), Duration::from_secs(300));
        assert_eq!(poller.backoff_delay(3), Duration::from_secs(600));
        assert_eq!(poller.backoff_delay(4), Duration::from_secs(900));
        assert_eq!(
            poller.backoff_delay(99),
            Duration::from_secs(900),
            "ladder caps at 15m"
        );
    }

    #[test]
    fn jitter_stays_within_ten_percent() {
        let base = Duration::from_secs(300);
        for i in 0..50 {
            let d = jittered(base, &id("a"), at(NOW_SECS + i));
            assert!(d >= base);
            assert!(d <= base + base.mul_f64(0.1));
        }
    }

    // ---- poll loop behavior (mock fetcher, no network, no sleeps) ----

    #[tokio::test]
    async fn successful_poll_records_usage_into_pool() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        let fetcher = MockFetcher::new(vec![Ok(snapshot_with(0.42))]);
        let mut poller =
            UsagePoller::with_fetcher(pool.clone(), &fetcher, "http://x".into(), config());
        poller.tick(now()).await;
        assert_eq!(fetcher.call_count(), 1);
        assert_eq!(
            fetcher.calls.lock().unwrap()[0],
            "at-a",
            "bearer = account token"
        );
        let snapshot = pool.snapshot();
        assert_eq!(snapshot.accounts[0].five_hour.unwrap().utilization, 0.42);
    }

    #[tokio::test]
    async fn apikey_accounts_are_never_polled() {
        let pool = AccountPool::new(&[apikey_account("k")]);
        let fetcher = MockFetcher::new(vec![]);
        let mut poller = UsagePoller::with_fetcher(pool, &fetcher, "http://x".into(), config());
        poller.tick(now()).await;
        assert_eq!(fetcher.call_count(), 0);
    }

    #[tokio::test]
    async fn respects_per_account_next_allowed_at() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        let fetcher = MockFetcher::new(vec![Ok(snapshot_with(0.1)), Ok(snapshot_with(0.2))]);
        let mut poller = UsagePoller::with_fetcher(pool, &fetcher, "http://x".into(), config());
        poller.tick(now()).await;
        assert_eq!(fetcher.call_count(), 1);
        // Immediately after: not due yet (interval 300s + jitter).
        poller.tick(at(NOW_SECS + 1)).await;
        assert_eq!(
            fetcher.call_count(),
            1,
            "second poll suppressed before next_at"
        );
        // Well past the jittered interval (300s + 10% max): due again.
        poller.tick(at(NOW_SECS + 331)).await;
        assert_eq!(fetcher.call_count(), 2);
    }

    #[tokio::test]
    async fn failures_climb_ladder_and_recover_on_success() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        let fetcher = MockFetcher::new(vec![
            Err(status_err(500)),
            Err(status_err(500)),
            Ok(snapshot_with(0.3)),
        ]);
        let mut poller = UsagePoller::with_fetcher(pool, &fetcher, "http://x".into(), config());

        poller.tick(now()).await; // failure #1 → next in ~120s
        assert_eq!(fetcher.call_count(), 1);
        let first_retry = poller.schedule[&id("a")];
        assert_eq!(first_retry.consecutive_failures, 1);
        let delay = first_retry.next_at.duration_since(now()).unwrap();
        assert!(delay >= Duration::from_secs(120) && delay <= Duration::from_secs(132));

        poller.tick(at(NOW_SECS + 133)).await; // failure #2 → next in ~300s
        let second_retry = poller.schedule[&id("a")];
        assert_eq!(second_retry.consecutive_failures, 2);
        let delay = second_retry
            .next_at
            .duration_since(at(NOW_SECS + 133))
            .unwrap();
        assert!(delay >= Duration::from_secs(300) && delay <= Duration::from_secs(330));

        poller.tick(at(NOW_SECS + 500)).await; // success → ladder resets
        let recovered = poller.schedule[&id("a")];
        assert_eq!(recovered.consecutive_failures, 0);
        assert_eq!(fetcher.call_count(), 3);
    }

    #[tokio::test]
    async fn forbidden_marks_auth_failure_unauthorized_does_not() {
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        let fetcher = MockFetcher::new(vec![Err(status_err(403)), Err(status_err(401))]);
        let mut poller =
            UsagePoller::with_fetcher(pool.clone(), &fetcher, "http://x".into(), config());
        // One poll per tick (MIN_POLL_GAP throttle): `a` this tick, `b` after
        // the gap. Together they cover both accounts without bursting.
        poller.tick(now()).await; // a → 403
        poller.tick(at(NOW_SECS + 11)).await; // b → 401
        let snapshot = pool.snapshot();
        let a = snapshot.accounts.iter().find(|x| x.id == id("a")).unwrap();
        let b = snapshot.accounts.iter().find(|x| x.id == id("b")).unwrap();
        assert!(!a.healthy, "403 = revoked → auth failure");
        assert!(b.healthy, "401 = expired token → refresh path owns it");
    }

    #[tokio::test]
    async fn removed_accounts_drop_their_schedule() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        let fetcher = MockFetcher::new(vec![Ok(snapshot_with(0.1))]);
        let mut poller =
            UsagePoller::with_fetcher(pool.clone(), &fetcher, "http://x".into(), config());
        poller.tick(now()).await;
        assert!(poller.schedule.contains_key(&id("a")));
        pool.reload_accounts(&[]);
        poller.tick(at(NOW_SECS + 1)).await;
        assert!(poller.schedule.is_empty());
    }
}
