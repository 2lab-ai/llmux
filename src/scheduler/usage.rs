//! Active quota tracking: per-account `GET /api/oauth/usage` poller with a
//! backoff ladder, so idle accounts (which produce no headers) still have
//! fresh window state.
//!
//! The HTTP call is injectable (`UsageFetcher`) so the scheduling logic is
//! testable without a network; `ReqwestFetcher` is the production impl.

use std::collections::HashMap;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde_json::Value;

use super::headers::{parse_epoch_seconds, parse_rfc3339, WindowReading};
use super::window::LimitSeverity;
use super::{AccountFingerprint, AccountId, AccountPool};
use crate::auth::gemini_usage::{self, GeminiUsageError};
use crate::auth::grok_usage::{self, GrokUsageError};
use crate::config::{AccountCredential, GeminiConfig, SchedulerConfig};
use crate::proxy::gemini_project::{self, GeminiProject};

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
    /// Grok billing read (`GET /billing?format=credits`) failed. Carries the
    /// module's SANITIZED phrase only — never a url, body or token.
    #[error("grok billing read failed: {0}")]
    GrokBilling(GrokUsageError),
    /// Gemini quota read (`POST /v1internal:fetchAvailableModels`) failed.
    /// Carries the module's SANITIZED phrase only — never a url, body or token.
    #[error("gemini quota read failed: {0}")]
    GeminiQuota(GeminiUsageError),
}

/// A grok billing failure becomes a poll failure. An HTTP status is lifted into
/// [`UsageError::Status`] so both providers share ONE status-shaped failure for
/// logging and the backoff ladder; the auth-failure VERDICT is per-source and
/// lives in [`UsagePoller::poll_account`] (grok billing statuses never bench).
impl From<GrokUsageError> for UsageError {
    fn from(err: GrokUsageError) -> Self {
        match err {
            GrokUsageError::Status { status } => UsageError::Status { status },
            other => UsageError::GrokBilling(other),
        }
    }
}

/// A gemini quota failure becomes a poll failure, exactly like grok's: an HTTP
/// status is lifted into [`UsageError::Status`], everything else keeps its
/// sanitized phrase. No gemini quota status benches (docs/gemini/spec.md §R6);
/// see [`UsagePoller::poll_account`].
impl From<GeminiUsageError> for UsageError {
    fn from(err: GeminiUsageError) -> Self {
        match err {
            GeminiUsageError::Status { status } => UsageError::Status { status },
            other => UsageError::GeminiQuota(other),
        }
    }
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
/// `claude:dev1@example.com`, 2026-07-03 (`.prd/13-usage-raw-sources.md`
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

/// Injectable transport for the usage endpoints, so the poller is testable
/// without a network. One method per usage SOURCE: anthropic oauth's
/// `/api/oauth/usage`, grok's billing endpoint and gemini's Cloud Code model
/// catalog are different documents on different hosts with different identity
/// headers.
pub trait UsageFetcher: Send + Sync {
    fn fetch(
        &self,
        base_url: &str,
        access_token: &str,
    ) -> impl Future<Output = Result<UsageSnapshot, UsageError>> + Send;

    /// `GET {upstream}/billing?format=credits` for one grok account
    /// (`crate::auth::grok_usage`). `subject` is the credential's `sub` claim,
    /// sent as `x-userid`.
    fn fetch_grok(
        &self,
        upstream: &str,
        access_token: &str,
        subject: &str,
    ) -> impl Future<Output = Result<UsageSnapshot, UsageError>> + Send;

    /// `POST {upstream}/v1internal:fetchAvailableModels {"project"}` for one
    /// gemini account (`crate::auth::gemini_usage`), sent with the configured
    /// Antigravity `user_agent`. Only the quota's usage windows are returned.
    fn fetch_gemini(
        &self,
        upstream: &str,
        user_agent: &str,
        access_token: &str,
        project: &str,
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

    fn fetch_grok(
        &self,
        upstream: &str,
        access_token: &str,
        subject: &str,
    ) -> impl Future<Output = Result<UsageSnapshot, UsageError>> + Send {
        let client = self.client.clone();
        let upstream = upstream.to_owned();
        let access_token = access_token.to_owned();
        let subject = subject.to_owned();
        async move {
            grok_usage::fetch_billing(&client, &upstream, &access_token, &subject)
                .await
                .map(|billing| billing.usage)
                .map_err(UsageError::from)
        }
    }

    fn fetch_gemini(
        &self,
        upstream: &str,
        user_agent: &str,
        access_token: &str,
        project: &str,
    ) -> impl Future<Output = Result<UsageSnapshot, UsageError>> + Send {
        let client = self.client.clone();
        let upstream = upstream.to_owned();
        let user_agent = user_agent.to_owned();
        let access_token = access_token.to_owned();
        let project = project.to_owned();
        async move {
            gemini_usage::fetch_quota(&client, &upstream, &user_agent, &access_token, &project)
                .await
                .map(|quota| quota.usage)
                .map_err(UsageError::from)
        }
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

/// What the poller needs to resolve a gemini account's missing Cloud Code
/// project beyond what it already holds (pool, Cloud Code base, identity):
/// the HTTP client (the fetcher is injectable and may not have one) and the
/// config file the project is persisted in.
struct GeminiProjects {
    client: reqwest::Client,
    config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
struct PollSchedule {
    next_at: SystemTime,
    consecutive_failures: u32,
    generation: u64,
    fresh_login: bool,
}

/// What actually happened at the fingerprint-guarded application boundary.
/// A transport result alone cannot prove a window landed or auth was excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollEffect {
    UsageApplied,
    AuthExcluded,
    Unchanged,
    Discarded,
    Skipped,
}

struct PollObservation {
    result: Result<(), UsageError>,
    effect: PollEffect,
}

impl PollObservation {
    fn skipped() -> Self {
        Self {
            result: Ok(()),
            effect: PollEffect::Skipped,
        }
    }

    fn history_event(
        &self,
        account: &AccountId,
        now: SystemTime,
    ) -> crate::account_history::AccountEvent {
        let status = match &self.result {
            Err(UsageError::Status { status }) => Some(status.as_u16()),
            Ok(()) if self.effect != PollEffect::Skipped => Some(200),
            _ => None,
        };
        let (outcome, reason, effect) = match self.effect {
            PollEffect::UsageApplied => ("success", "usage snapshot applied", "auth unchanged"),
            PollEffect::AuthExcluded => (
                "excluded",
                "Claude usage endpoint forbidden; auth exclusion applied",
                "auth_failed",
            ),
            PollEffect::Discarded => (
                "discarded",
                "usage response belongs to retired credentials",
                "auth and usage unchanged",
            ),
            PollEffect::Skipped => (
                "skipped",
                "usage source unavailable; no request sent",
                "auth and usage unchanged",
            ),
            PollEffect::Unchanged => (
                "failed",
                match &self.result {
                    Err(UsageError::Status { status }) => match status.as_u16() {
                        401 => "access token rejected; token refresh owns recovery",
                        403 => {
                            "usage endpoint forbidden; informational source leaves auth unchanged"
                        }
                        429 => "usage endpoint rate limited; authentication unchanged",
                        _ => "usage endpoint error; authentication unchanged",
                    },
                    Err(UsageError::Parse(_)) => "usage response could not be parsed",
                    _ => "usage transport/provider read failed",
                },
                "auth unchanged",
            ),
        };
        crate::account_history::AccountEvent::new(
            &account.0,
            "usage_poll",
            outcome,
            status,
            reason,
            effect,
            now,
        )
    }
}

/// Background poller: polls every account that HAS a usage source (anthropic
/// oauth via `/api/oauth/usage`, grok via the billing endpoint, gemini via the
/// Cloud Code model catalog) at `usage_poll_secs` cadence with jitter; failures
/// climb the backoff ladder and recover on first success. Each account has its
/// own next-allowed-at; API-key / codex / openrouter accounts are skipped (no
/// poller usage source — codex refreshes through the explicit usage-control
/// path).
pub struct UsagePoller<F = ReqwestFetcher> {
    pool: AccountPool,
    fetcher: F,
    base_url: String,
    /// `config.grok.upstream` — the base the billing URL is derived from. An
    /// unusable value is REFUSED by `grok_usage::billing_url`, never replaced
    /// with the production default.
    grok_upstream: String,
    /// `config.gemini.upstream` — the Cloud Code base the quota read is posted
    /// to. [`GeminiConfig::default`] until [`Self::with_gemini`] sets it.
    gemini_upstream: String,
    /// `config.gemini.user_agent` — the Antigravity identity the quota read
    /// carries. [`GeminiConfig::default`] until [`Self::with_gemini`] sets it.
    gemini_user_agent: String,
    /// The handle a missing gemini project is resolved through (the HTTP
    /// client + the config file it is persisted in), set by
    /// [`Self::with_gemini_projects`]. `None` keeps the skip: an account
    /// without a project has no usage source.
    gemini_projects: Option<GeminiProjects>,
    config: SchedulerConfig,
    schedule: HashMap<AccountId, PollSchedule>,
    /// Wall-clock time of the last poll, for the global [`MIN_POLL_GAP`] throttle.
    last_poll_at: Option<SystemTime>,
    /// Best-effort poller-health feed to the dashboard (`try_send`, dropped
    /// on a full channel — same contract as every activity sender).
    events: Option<tokio::sync::mpsc::Sender<crate::tui::ActivityEvent>>,
    hub: Option<std::sync::Arc<crate::dashboard::DashboardHub>>,
}

impl UsagePoller<ReqwestFetcher> {
    pub fn new(
        pool: AccountPool,
        client: reqwest::Client,
        base_url: String,
        grok_upstream: String,
        config: SchedulerConfig,
    ) -> Self {
        Self::with_fetcher(
            pool,
            ReqwestFetcher { client },
            base_url,
            grok_upstream,
            config,
        )
    }
}

impl<F: UsageFetcher> UsagePoller<F> {
    /// Build a poller with a custom transport (tests inject a mock here).
    pub fn with_fetcher(
        pool: AccountPool,
        fetcher: F,
        base_url: String,
        grok_upstream: String,
        config: SchedulerConfig,
    ) -> Self {
        let gemini = GeminiConfig::default();
        Self {
            pool,
            fetcher,
            base_url,
            grok_upstream,
            gemini_upstream: gemini.upstream,
            gemini_user_agent: gemini.user_agent,
            gemini_projects: None,
            config,
            schedule: HashMap::new(),
            last_poll_at: None,
            events: None,
            hub: None,
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

    pub fn with_hub(mut self, hub: std::sync::Arc<crate::dashboard::DashboardHub>) -> Self {
        self.hub = Some(hub);
        self
    }

    /// Read gemini quota from the CONFIGURED Cloud Code base with the
    /// configured Antigravity user agent (`config.gemini.upstream` /
    /// `.user_agent`) instead of the [`GeminiConfig::default`] values.
    pub fn with_gemini(mut self, upstream: String, user_agent: String) -> Self {
        self.gemini_upstream = upstream;
        self.gemini_user_agent = user_agent;
        self
    }

    /// Resolve a gemini account's missing Cloud Code project before its
    /// quota read (an operator who only watches usage never sends the request
    /// that would resolve it), through `client`, persisting it in
    /// `config_path` — the same guarded write + persist as the request path.
    /// Scheduled polls only: the startup [`Self::prime`] pass never looks up.
    pub fn with_gemini_projects(
        mut self,
        client: reqwest::Client,
        config_path: Option<PathBuf>,
    ) -> Self {
        self.gemini_projects = Some(GeminiProjects {
            client,
            config_path,
        });
        self
    }

    /// Run forever (spawned as a background task next to the proxy server).
    pub async fn run(mut self) {
        loop {
            self.tick(SystemTime::now()).await;
            tokio::time::sleep(POLL_TICK).await;
        }
    }

    /// Re-read the POLLABLE roster (the credential kinds that have an active
    /// usage source), drop schedules for removed accounts, and give every
    /// current account a schedule entry (new accounts due immediately).
    fn refresh_schedule(&mut self, now: SystemTime) -> Vec<AccountId> {
        let pollable: Vec<AccountId> = self
            .pool
            .snapshot()
            .accounts
            .iter()
            .filter(|a| matches!(a.credential_kind, "oauth" | "grok" | "gemini"))
            .map(|a| a.id.clone())
            .collect();
        self.schedule.retain(|id, _| pollable.contains(id));
        for id in &pollable {
            let Some(fingerprint) = self.pool.fingerprint(id) else {
                continue;
            };
            // Roster replacement changes generation; ordinary token rotation
            // deliberately does not. A new login must not inherit old backoff.
            if self
                .schedule
                .get(id)
                .is_none_or(|e| e.generation != fingerprint.generation)
            {
                self.schedule.insert(
                    id.clone(),
                    PollSchedule {
                        next_at: now,
                        consecutive_failures: 0,
                        generation: fingerprint.generation,
                        fresh_login: true,
                    },
                );
                if let Some(hub) = &self.hub {
                    hub.reset_poll_health(&id.0, fingerprint.generation, now);
                }
            }
        }
        pollable
    }

    /// Poll one account and reschedule it (jittered interval on success, backoff
    /// ladder on failure), emitting a poller-health event. `resolve_projects`
    /// as in [`Self::poll`].
    async fn poll_and_reschedule(
        &mut self,
        id: AccountId,
        now: SystemTime,
        resolve_projects: bool,
    ) {
        let Some(expected) = self.pool.fingerprint(&id) else {
            return;
        };
        let prev_failures = self.schedule.get(&id).map_or(0, |e| e.consecutive_failures);
        let observation = self.poll(&id, now, resolve_projects).await;
        // Generation controls re-login scheduling. The exact fingerprint's
        // applied/discarded result controls receipts, including automatic
        // rotation within one generation. A discarded reply changes neither
        // the current poll health nor the existing backoff/deadline.
        if observation.effect == PollEffect::Discarded
            || self
                .pool
                .fingerprint(&id)
                .is_none_or(|current| current.generation != expected.generation)
        {
            if let Some(hub) = &self.hub {
                hub.account_history
                    .record(observation.history_event(&id, now));
            }
            self.refresh_schedule(now);
            return;
        }
        let failures = match &observation.result {
            Ok(()) => 0,
            Err(err) => {
                tracing::warn!(account = %id, error = %err, "usage poll failed");
                prev_failures.saturating_add(1)
            }
        };
        let delay = jittered(self.backoff_delay(failures), &id, now);
        if let Some(hub) = &self.hub {
            hub.account_history.record(
                observation
                    .history_event(&id, now)
                    .with_retry(now + delay, failures),
            );
        }
        // No successful read occurred when no source/project was available.
        // Keep the existing retry cadence without inventing a last_ok stamp.
        if observation.effect != PollEffect::Skipped {
            if let Some(events) = &self.events {
                let _ = events.try_send(crate::tui::ActivityEvent::UsagePolled {
                    account: id.0.clone(),
                    generation: expected.generation,
                    ok: failures == 0,
                    consecutive_failures: failures,
                    next_in: delay,
                });
            }
        }
        self.schedule.insert(
            id,
            PollSchedule {
                next_at: now + delay,
                consecutive_failures: failures,
                generation: expected.generation,
                fresh_login: false,
            },
        );
    }

    /// Startup priming: poll EVERY due account once so the first selection ranks
    /// on real window data. This is a one-time burst at boot; the ongoing
    /// [`Self::tick`] throttles to one poll per [`MIN_POLL_GAP`] so the poller
    /// never *continuously* bursts a call per account (which can trip the
    /// upstream's org/IP request-rate limit).
    ///
    /// The daemon reports ready only after this pass, so it never runs a
    /// gemini project lookup (`loadCodeAssist`/`onboardUser` can take tens of
    /// seconds): a project-less account is skipped here exactly as before and
    /// resolves on its next scheduled poll ([`Self::tick`]).
    pub async fn prime(&mut self, now: SystemTime) {
        for id in self.refresh_schedule(now) {
            if self.schedule.get(&id).is_some_and(|e| e.next_at <= now) {
                self.poll_and_reschedule(id, now, false).await;
            }
        }
        self.last_poll_at = Some(now);
    }

    /// One scheduling pass: poll AT MOST ONE due account (the most overdue),
    /// throttled to one poll per [`MIN_POLL_GAP`] across all accounts. Re-reads
    /// the roster each pass so account reloads are picked up; removed accounts
    /// drop their schedule entries.
    pub async fn tick(&mut self, now: SystemTime) {
        let pollable = self.refresh_schedule(now);

        // Global throttle: at most one poll per MIN_POLL_GAP, so a pass that
        // finds many accounts due never bursts a call per account.
        if self
            .last_poll_at
            .is_some_and(|last| now.duration_since(last).is_ok_and(|gap| gap < MIN_POLL_GAP))
        {
            return;
        }

        // Poll the single most-overdue due account this tick.
        let Some(id) = pollable
            .iter()
            .filter(|id| self.schedule.get(*id).is_some_and(|e| e.next_at <= now))
            .min_by_key(|id| {
                self.schedule
                    .get(*id)
                    .map(|e| (!e.fresh_login, e.next_at))
                    .unwrap_or((true, now))
            })
            .cloned()
        else {
            return;
        };
        self.last_poll_at = Some(now);
        self.poll_and_reschedule(id, now, true).await;
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

    /// Poll a single account once and record the outcome. Accounts without a
    /// usage source (or vanished ones) are a no-op. A 401 is left for the auth
    /// layer's refresh path (the next poll retries with the refreshed
    /// credential) on BOTH sources: benching a serving account because an
    /// INFORMATIONAL gauge read hit an expired access token would cost
    /// capacity, and `forward.rs` `classify` (src/proxy/forward.rs:145)
    /// already refreshes-then-benches on a request-path 401.
    ///
    /// A 403 is a REVOKED-token verdict on the anthropic usage endpoint only
    /// (`bench_on_forbidden`). The grok billing endpoint has only ever been
    /// observed answering 200 and 401 (`docs/grok/spec.md` §R3), so its 403 is
    /// an unverified shape — a host WAF/challenge would produce one — and
    /// benching a healthy account on it is unrecoverable (`AccountHealth::
    /// AuthFailed` is sticky until re-login, src/scheduler/mod.rs:757-764).
    /// Real grok revocation is still caught on the request path by the same
    /// `classify` hop, so the grok billing read stays informational: 403/5xx
    /// climb the backoff ladder like any other poll failure.
    ///
    /// The gemini quota read is informational for the same reason: no status
    /// it answers ever benches (docs/gemini/spec.md §R6) — the ToS-suspension
    /// 403 is a request-path verdict. A gemini account whose Cloud Code
    /// project is not resolved yet (offline import) resolves it first when
    /// its token is usable ([`Self::with_gemini_projects`]; never in the
    /// startup [`Self::prime`] pass); until then it has no usage source: the
    /// poll is skipped as a success, recording no window and climbing no
    /// ladder.
    ///
    /// Both outcomes are applied through the FINGERPRINT-GUARDED pool calls,
    /// so a verdict earned by a credential that was replaced mid-poll (a
    /// re-login) is discarded instead of landing on its successor.
    pub async fn poll_account(
        &self,
        account: &AccountId,
        now: SystemTime,
    ) -> Result<(), UsageError> {
        self.poll(account, now, true).await.result
    }

    /// [`Self::poll_account`]; `resolve_projects = false` (the startup
    /// [`Self::prime`] pass) skips the gemini project lookup and keeps the
    /// no-project skip.
    async fn poll(
        &self,
        account: &AccountId,
        now: SystemTime,
        resolve_projects: bool,
    ) -> PollObservation {
        // Credential AND fingerprint in ONE capture: everything below is
        // applied only if the account still IS what was read here
        // (`docs/keys-history/relogin-trace.md` B4). A re-login that lands
        // during the fetch must not receive this poll's verdict.
        let Some((mut credential, mut fingerprint)) =
            self.pool.credential_with_fingerprint(account)
        else {
            return PollObservation::skipped();
        };
        // A gemini account still waiting for its project resolves it here,
        // at most once per poll of this account (never in `prime`); the
        // reading then belongs to the resolved credential (its post-write
        // fingerprint).
        let resolved = if resolve_projects {
            self.resolve_gemini_project(account, &credential).await
        } else {
            None
        };
        if let Some((resolved, resolved_fingerprint)) = resolved {
            credential = resolved;
            fingerprint = resolved_fingerprint;
        }
        // One usage SOURCE per credential kind. A grok snapshot carries only
        // the weekly window, and `PoolState::record_usage` merges `Some`
        // windows only, so the header-fed 5h burst gauge stays intact.
        // `bench_on_forbidden` rides along because the 403 verdict belongs to
        // the SOURCE, not to the account (see the doc comment above).
        let (outcome, bench_on_forbidden) = match &credential {
            AccountCredential::Oauth { access_token, .. } => {
                (self.fetcher.fetch(&self.base_url, access_token).await, true)
            }
            AccountCredential::Grok {
                access_token,
                subject,
                ..
            } => (
                self.fetcher
                    .fetch_grok(&self.grok_upstream, access_token, subject)
                    .await,
                false,
            ),
            AccountCredential::Gemini {
                access_token,
                project_id,
                ..
            } => {
                // Still no project (the startup pass, no resolver wired, the
                // token not usable for the lookup, or the lookup failed): no
                // usage source yet — never a fabricated window.
                if project_id.is_empty() {
                    return PollObservation::skipped();
                }
                (
                    self.fetcher
                        .fetch_gemini(
                            &self.gemini_upstream,
                            &self.gemini_user_agent,
                            access_token,
                            project_id,
                        )
                        .await,
                    false,
                )
            }
            _ => return PollObservation::skipped(),
        };
        match outcome {
            Ok(snapshot) => {
                let applied = self
                    .pool
                    .record_usage_if(account, &fingerprint, &snapshot, now);
                PollObservation {
                    result: Ok(()),
                    effect: if applied {
                        PollEffect::UsageApplied
                    } else {
                        PollEffect::Discarded
                    },
                }
            }
            Err(err) => {
                let forbidden = matches!(
                    &err,
                    UsageError::Status { status } if *status == http::StatusCode::FORBIDDEN
                );
                let effect = if bench_on_forbidden && forbidden {
                    if self.pool.record_auth_failure_if(
                        account,
                        &fingerprint,
                        Some(http::StatusCode::FORBIDDEN),
                    ) {
                        PollEffect::AuthExcluded
                    } else {
                        PollEffect::Discarded
                    }
                } else if self.pool.fingerprint(account).as_ref() != Some(&fingerprint) {
                    PollEffect::Discarded
                } else {
                    PollEffect::Unchanged
                };
                PollObservation {
                    result: Err(err),
                    effect,
                }
            }
        }
    }

    /// [`crate::proxy::gemini_project::ensure_project`] for a gemini account
    /// that [awaits a lookup](crate::proxy::gemini_project::awaits_lookup),
    /// when [`Self::with_gemini_projects`] wired the handle. `None` = keep
    /// today's skip: not such an account, no handle, a re-login in flight
    /// (its successor is polled next), or the lookup failed — logged, never
    /// retried before this account's next scheduled poll, and never an
    /// error: a missing usage source does not climb the backoff ladder.
    async fn resolve_gemini_project(
        &self,
        account: &AccountId,
        credential: &AccountCredential,
    ) -> Option<(AccountCredential, AccountFingerprint)> {
        let projects = self.gemini_projects.as_ref()?;
        if !gemini_project::awaits_lookup(credential) {
            return None;
        }
        let lookup = gemini_project::ProjectLookup {
            client: &projects.client,
            upstream: &self.gemini_upstream,
            user_agent: &self.gemini_user_agent,
            pool: &self.pool,
            config_path: projects.config_path.as_deref(),
        };
        match gemini_project::ensure_project(&lookup, account, credential).await {
            GeminiProject::Resolved {
                credential,
                fingerprint,
            } => Some((credential, fingerprint)),
            GeminiProject::Ready | GeminiProject::Superseded => None,
            GeminiProject::Failed(err) => {
                tracing::warn!(account = %account,
                    error = %gemini_project::usage_failure_message(&err),
                    "gemini usage poll skipped: project not resolved");
                None
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

    fn grok_account(name: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            credential: AccountCredential::Grok {
                subject: format!("sub-{name}"),
                access_token: format!("at-{name}"),
                refresh_token: format!("rt-{name}"),
                expires_at_ms: 0,
                token_endpoint: String::new(),
                last_refresh_ms: None,
            },
        }
    }

    /// Never the production `cli-chat-proxy.grok.com` — the mock fetcher does
    /// not dial, but the value is what `poll_account` forwards.
    const GROK_UPSTREAM: &str = "http://grok.invalid/v1";

    /// A gemini account; `project` empty = not yet resolved (offline import).
    fn gemini_account(name: &str, project: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            credential: AccountCredential::Gemini {
                email: format!("{name}@example.com"),
                access_token: format!("at-{name}"),
                refresh_token: format!("rt-{name}"),
                expires_at_ms: 0,
                project_id: project.to_string(),
                last_refresh_ms: None,
            },
        }
    }

    /// Never the production Cloud Code host — same rule as [`GROK_UPSTREAM`].
    const GEMINI_UPSTREAM: &str = "http://gemini.invalid";
    const GEMINI_USER_AGENT: &str = "antigravity/test";

    fn config() -> SchedulerConfig {
        SchedulerConfig::default() // poll 300s, max age 600s
    }

    /// Scripted fetcher: pops the next queued result per call and records
    /// the tokens it was called with. Every usage source shares the `results`
    /// queue; the grok and gemini calls are recorded separately so a test can
    /// prove WHICH endpoint an account was read through.
    struct MockFetcher {
        results: Mutex<Vec<Result<UsageSnapshot, UsageError>>>,
        calls: Mutex<Vec<String>>,
        grok_calls: Mutex<Vec<(String, String, String)>>,
        /// `(upstream, user_agent, access_token, project)` per gemini read.
        gemini_calls: Mutex<Vec<(String, String, String, String)>>,
    }

    impl MockFetcher {
        fn new(results: Vec<Result<UsageSnapshot, UsageError>>) -> Self {
            Self {
                results: Mutex::new(results),
                calls: Mutex::new(Vec::new()),
                grok_calls: Mutex::new(Vec::new()),
                gemini_calls: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }

        fn next_result(&self) -> Result<UsageSnapshot, UsageError> {
            let mut results = self.results.lock().unwrap();
            if results.is_empty() {
                Ok(UsageSnapshot::default())
            } else {
                results.remove(0)
            }
        }
    }

    impl UsageFetcher for &MockFetcher {
        fn fetch(
            &self,
            _base_url: &str,
            access_token: &str,
        ) -> impl Future<Output = Result<UsageSnapshot, UsageError>> + Send {
            self.calls.lock().unwrap().push(access_token.to_string());
            let result = self.next_result();
            async move { result }
        }

        fn fetch_grok(
            &self,
            upstream: &str,
            access_token: &str,
            subject: &str,
        ) -> impl Future<Output = Result<UsageSnapshot, UsageError>> + Send {
            self.grok_calls.lock().unwrap().push((
                upstream.to_string(),
                access_token.to_string(),
                subject.to_string(),
            ));
            let result = self.next_result();
            async move { result }
        }

        fn fetch_gemini(
            &self,
            upstream: &str,
            user_agent: &str,
            access_token: &str,
            project: &str,
        ) -> impl Future<Output = Result<UsageSnapshot, UsageError>> + Send {
            self.gemini_calls.lock().unwrap().push((
                upstream.to_string(),
                user_agent.to_string(),
                access_token.to_string(),
                project.to_string(),
            ));
            let result = self.next_result();
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
        let poller = UsagePoller::with_fetcher(
            pool,
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );
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
    async fn relogin_retries_usage_without_inheriting_backoff() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        let fetcher = MockFetcher::new(vec![Err(status_err(429)), Ok(snapshot_with(0.42))]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );
        poller.tick(now()).await;
        assert_eq!(poller.schedule[&id("a")].consecutive_failures, 1);
        let mut fresh = oauth_account("a");
        if let AccountCredential::Oauth { access_token, .. } = &mut fresh.credential {
            *access_token = "at-relogin".into();
        }
        pool.reload_accounts(&[fresh]);
        poller.tick(now() + MIN_POLL_GAP).await;
        assert_eq!(
            fetcher.call_count(),
            2,
            "new login must not wait out old 429 backoff"
        );
        assert_eq!(poller.schedule[&id("a")].consecutive_failures, 0);
        assert_eq!(
            pool.snapshot().accounts[0].five_hour.unwrap().utilization,
            0.42
        );
    }

    #[tokio::test]
    async fn automatic_token_rotation_preserves_usage_backoff() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        let fetcher = MockFetcher::new(vec![Err(status_err(429))]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );
        poller.tick(now()).await;
        let due = poller.schedule[&id("a")].next_at;
        let mut fresh = oauth_account("a");
        if let AccountCredential::Oauth { access_token, .. } = &mut fresh.credential {
            *access_token = "rotated".into();
        }
        pool.update_credential(&id("a"), fresh.credential);
        poller.tick(now() + MIN_POLL_GAP).await;
        assert_eq!(
            fetcher.call_count(),
            1,
            "ordinary refresh must not bypass 429 pacing"
        );
        assert_eq!(poller.schedule[&id("a")].next_at, due);
        assert_eq!(poller.schedule[&id("a")].consecutive_failures, 1);
    }

    #[tokio::test]
    async fn relogin_fresh_poll_failure_starts_new_backoff_not_fake_success() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        let fetcher = MockFetcher::new(vec![Err(status_err(429)), Err(status_err(429))]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );
        poller.tick(now()).await;
        let mut fresh = oauth_account("a");
        if let AccountCredential::Oauth { access_token, .. } = &mut fresh.credential {
            *access_token = "new-login".into();
        }
        pool.reload_accounts(&[fresh]);
        poller.tick(now() + MIN_POLL_GAP).await;
        assert_eq!(fetcher.call_count(), 2);
        assert_eq!(poller.schedule[&id("a")].consecutive_failures, 1);
        assert!(pool.snapshot().accounts[0].five_hour.is_none());
        assert!(
            pool.snapshot().accounts[0].healthy,
            "429 does not revoke auth"
        );
    }

    #[tokio::test]
    async fn fresh_login_is_next_even_when_other_accounts_are_overdue() {
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        let fetcher = MockFetcher::new(vec![]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );
        poller.prime(now()).await;
        poller.schedule.get_mut(&id("a")).unwrap().next_at = now() - Duration::from_secs(60);
        let mut b = oauth_account("b");
        if let AccountCredential::Oauth { access_token, .. } = &mut b.credential {
            *access_token = "new-b".into();
        }
        pool.reload_accounts(&[oauth_account("a"), b]);
        poller.tick(now() + MIN_POLL_GAP).await;
        assert_eq!(
            fetcher.calls.lock().unwrap().last().unwrap(),
            "new-b",
            "fresh login is queued ahead of old overdue polls, without bypassing global pacing"
        );
    }

    #[tokio::test]
    async fn successful_poll_records_usage_into_pool() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        let fetcher = MockFetcher::new(vec![Ok(snapshot_with(0.42))]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );
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
        let mut poller = UsagePoller::with_fetcher(
            pool,
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );
        poller.tick(now()).await;
        assert_eq!(fetcher.call_count(), 0);
    }

    #[tokio::test]
    async fn respects_per_account_next_allowed_at() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        let fetcher = MockFetcher::new(vec![Ok(snapshot_with(0.1)), Ok(snapshot_with(0.2))]);
        let mut poller = UsagePoller::with_fetcher(
            pool,
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );
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
        let mut poller = UsagePoller::with_fetcher(
            pool,
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );

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
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );
        // One poll per tick (MIN_POLL_GAP throttle): `a` this tick, `b` after
        // the gap. Together they cover both accounts without bursting.
        poller.tick(now()).await; // a → 403
        poller.tick(at(NOW_SECS + 11)).await; // b → 401
        let snapshot = pool.snapshot();
        let a = snapshot.accounts.iter().find(|x| x.id == id("a")).unwrap();
        let b = snapshot.accounts.iter().find(|x| x.id == id("b")).unwrap();
        assert!(!a.healthy, "403 = revoked → auth failure");
        assert_eq!(a.auth_failure_status, Some(403));
        assert!(b.healthy, "401 = expired token → refresh path owns it");
    }

    /// Replace credentials during the provider read, either by roster reload
    /// (re-login) or ordinary token rotation, exercising real pool guards.
    struct CredentialChangeFetcher {
        pool: AccountPool,
        rotate_only: bool,
        result: Mutex<Option<Result<UsageSnapshot, UsageError>>>,
    }

    impl CredentialChangeFetcher {
        /// Re-login mid-fetch: the roster is replaced with a fresh credential
        /// before the scripted answer is handed back.
        fn relogin(&self) -> Result<UsageSnapshot, UsageError> {
            let mut relogged = oauth_account("a");
            if let AccountCredential::Oauth { access_token, .. } = &mut relogged.credential {
                *access_token = "at-a-relogin".into();
            }
            if self.rotate_only {
                self.pool.update_credential(&id("a"), relogged.credential);
            } else {
                self.pool.reload_accounts(&[relogged]);
            }
            self.result
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Ok(UsageSnapshot::default()))
        }
    }

    impl UsageFetcher for &CredentialChangeFetcher {
        fn fetch(
            &self,
            _base_url: &str,
            _access_token: &str,
        ) -> impl Future<Output = Result<UsageSnapshot, UsageError>> + Send {
            let result = self.relogin();
            async move { result }
        }

        fn fetch_grok(
            &self,
            _upstream: &str,
            _access_token: &str,
            _subject: &str,
        ) -> impl Future<Output = Result<UsageSnapshot, UsageError>> + Send {
            let result = self.relogin();
            async move { result }
        }

        fn fetch_gemini(
            &self,
            _upstream: &str,
            _user_agent: &str,
            _access_token: &str,
            _project: &str,
        ) -> impl Future<Output = Result<UsageSnapshot, UsageError>> + Send {
            let result = self.relogin();
            async move { result }
        }
    }

    #[tokio::test]
    async fn usage_history_discards_rotated_token_results_without_changing_poll_health() {
        for result in [Err(status_err(403)), Ok(snapshot_with(0.9))] {
            let pool = AccountPool::new(&[oauth_account("a")]);
            let generation = pool.fingerprint(&id("a")).unwrap().generation;
            let fetcher = CredentialChangeFetcher {
                pool: pool.clone(),
                rotate_only: true,
                result: Mutex::new(Some(result)),
            };
            let hub = std::sync::Arc::new(crate::dashboard::DashboardHub::default());
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            let mut poller = UsagePoller::with_fetcher(
                pool.clone(),
                &fetcher,
                "http://x".into(),
                GROK_UPSTREAM.into(),
                config(),
            )
            .with_hub(hub.clone())
            .with_events(Some(tx));
            poller.tick(now()).await;
            while let Ok(event) = rx.try_recv() {
                hub.apply_event(event, now());
            }
            assert_eq!(pool.fingerprint(&id("a")).unwrap().generation, generation);
            assert!(pool.snapshot().accounts[0].healthy);
            assert!(pool.snapshot().accounts[0].five_hour.is_none());
            let rows = hub.account_history.snapshot();
            let event = &rows["a"][0];
            assert_eq!(event.outcome, "discarded", "{event:?}");
            assert!(!event.effect.contains("auth_failed"), "{event:?}");
            assert_eq!(poller.schedule[&id("a")].consecutive_failures, 0);
            assert_eq!(poller.schedule[&id("a")].next_at, now());
            assert_eq!(hub.view(now()).poll_health["a"].last_ok, None);
            assert_eq!(hub.view(now()).poll_health["a"].consecutive_failures, 0);
        }
    }

    #[tokio::test]
    async fn usage_history_forbidden_reports_the_applied_provider_policy() {
        for (account, excluded) in [
            (grok_account("a"), false),
            (gemini_account("a", "project-a"), false),
            (oauth_account("a"), true),
        ] {
            let pool = AccountPool::new(&[account]);
            let fetcher = MockFetcher::new(vec![Err(status_err(403))]);
            let hub = std::sync::Arc::new(crate::dashboard::DashboardHub::default());
            let mut poller = UsagePoller::with_fetcher(
                pool.clone(),
                &fetcher,
                "http://x".into(),
                GROK_UPSTREAM.into(),
                config(),
            )
            .with_hub(hub.clone());
            poller.tick(now()).await;
            assert_eq!(pool.snapshot().accounts[0].healthy, !excluded);
            let rows = hub.account_history.snapshot();
            let event = &rows["a"][0];
            assert_eq!(event.http_status, Some(403));
            if excluded {
                assert_eq!(event.outcome, "excluded");
                assert_eq!(event.effect, "auth_failed");
            } else {
                assert_eq!(event.outcome, "failed");
                assert_eq!(event.effect, "auth unchanged");
                assert!(!event.reason.contains("excluded"), "{event:?}");
                assert!(event.reason.contains("informational"), "{event:?}");
            }
        }
    }

    #[tokio::test]
    async fn usage_history_skipped_project_has_no_http_or_successful_poll() {
        let pool = AccountPool::new(&[gemini_account("a", "")]);
        let fetcher = MockFetcher::new(vec![]);
        let hub = std::sync::Arc::new(crate::dashboard::DashboardHub::default());
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        )
        .with_hub(hub.clone())
        .with_events(Some(tx));
        poller.tick(now()).await;
        while let Ok(event) = rx.try_recv() {
            hub.apply_event(event, now());
        }
        assert!(fetcher.gemini_calls.lock().unwrap().is_empty());
        let rows = hub.account_history.snapshot();
        let event = &rows["a"][0];
        assert_eq!(event.outcome, "skipped", "{event:?}");
        assert_eq!(event.http_status, None, "no upstream request was sent");
        assert!(pool.snapshot().accounts[0].seven_day.is_none());
        assert_eq!(hub.view(now()).poll_health["a"].last_ok, None);
        assert_eq!(poller.schedule[&id("a")].consecutive_failures, 0);
    }

    #[tokio::test]
    async fn retired_poll_cannot_redegrade_or_backoff_new_login() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        let fetcher = CredentialChangeFetcher {
            pool: pool.clone(),
            rotate_only: false,
            result: Mutex::new(Some(Err(status_err(403)))),
        };
        let hub = std::sync::Arc::new(crate::dashboard::DashboardHub::default());
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        )
        .with_events(Some(tx))
        .with_hub(hub.clone());
        poller.tick(now()).await;
        assert_eq!(poller.schedule[&id("a")].consecutive_failures, 0);
        assert_eq!(poller.schedule[&id("a")].next_at, now());
        assert!(
            rx.try_recv().is_err(),
            "no stale UsagePolled verdict is enqueued"
        );
        assert_eq!(hub.view(now()).poll_health["a"].consecutive_failures, 0);
        assert_eq!(hub.account_history.snapshot()["a"][0].outcome, "discarded");
        assert!(pool.snapshot().accounts[0].healthy);
    }

    /// B4: a 403 earned by the credential the re-login retired must not bench
    /// the account the re-login just healed.
    #[tokio::test]
    async fn forbidden_from_a_retired_credential_does_not_bench_the_relogin() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        pool.record_auth_failure(&id("a"), None);
        let fetcher = CredentialChangeFetcher {
            pool: pool.clone(),
            rotate_only: false,
            result: Mutex::new(Some(Err(status_err(403)))),
        };
        let poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );

        let _ = poller.poll_account(&id("a"), now()).await;

        assert!(
            pool.snapshot().accounts[0].healthy,
            "the re-logged-in account stays healthy"
        );
    }

    /// B4, the success half: a reading fetched with the retired credential is
    /// not the new credential's usage, so it is discarded rather than merged.
    #[tokio::test]
    async fn usage_read_with_a_retired_credential_is_discarded() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        let fetcher = CredentialChangeFetcher {
            pool: pool.clone(),
            rotate_only: false,
            result: Mutex::new(Some(Ok(snapshot_with(0.9)))),
        };
        let poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );

        poller.poll_account(&id("a"), now()).await.expect("poll ok");

        assert!(
            pool.snapshot().accounts[0].five_hour.is_none(),
            "a reading from the retired credential never lands"
        );
    }

    // ---- grok billing source (docs/grok/spec.md §R3) ----

    /// A weekly billing reading from the grok billing endpoint.
    fn grok_weekly(util: f64) -> UsageSnapshot {
        UsageSnapshot {
            five_hour: None,
            seven_day: Some(WindowReading {
                utilization: util,
                resets_at: at(NOW_SECS + 604_800),
            }),
            scoped: Vec::new(),
        }
    }

    /// Grok accounts USED to be skipped by the poller (only the `Oauth` arm
    /// existed), so their 7d gauge stayed empty forever. They are now polled
    /// through the billing endpoint — with the grok upstream, the account's own
    /// token and its `subject` — and the reading lands on `seven_day` WITHOUT
    /// disturbing the header-fed 5h burst gauge.
    #[tokio::test]
    async fn grok_accounts_are_polled_via_billing() {
        let pool = AccountPool::new(&[grok_account("g")]);
        // The 5h slot as the response headers left it (grok's burst gauge).
        pool.record_usage(&id("g"), &snapshot_with(0.42), now());
        let fetcher = MockFetcher::new(vec![Ok(grok_weekly(0.65))]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );

        poller.tick(now()).await;

        assert_eq!(
            fetcher.call_count(),
            0,
            "a grok account never hits the anthropic usage endpoint"
        );
        let grok_calls = fetcher.grok_calls.lock().unwrap().clone();
        assert_eq!(
            grok_calls,
            vec![(
                GROK_UPSTREAM.to_string(),
                "at-g".to_string(),
                "sub-g".to_string()
            )],
            "billing read carries the grok upstream, the account token and its subject"
        );
        let account = pool.snapshot().accounts[0].clone();
        assert!(
            (account.seven_day.expect("7d gauge").utilization - 0.65).abs() < 1e-9,
            "the weekly allowance fills the 7d gauge"
        );
        assert_eq!(
            account.five_hour.expect("5h gauge").utilization,
            0.42,
            "a snapshot without a 5h window leaves the header-fed burst gauge intact"
        );
    }

    /// UNLIKE the oauth twin, NO billing status benches a grok account. The
    /// endpoint has only ever been observed answering 200 and 401, so a 403
    /// from that host is an unverified shape (a WAF/challenge answers that way)
    /// while `AccountHealth::AuthFailed` is sticky until re-login
    /// (src/scheduler/mod.rs:757-764) — an INFORMATIONAL gauge read may not
    /// permanently retire a serving account. Real revocation is still caught on
    /// the request path (`classify`, src/proxy/forward.rs:145). Both statuses
    /// are plain poll failures: backoff climbs and the previous reading stays.
    #[tokio::test]
    async fn grok_billing_forbidden_is_a_poll_failure_not_an_auth_failure() {
        let pool = AccountPool::new(&[grok_account("g"), grok_account("h")]);
        // A weekly reading each account already earned: a failed poll must
        // retain it (unknown is never zero, and neither is it cleared).
        pool.record_usage(&id("g"), &grok_weekly(0.65), now());
        pool.record_usage(&id("h"), &grok_weekly(0.65), now());
        let fetcher = MockFetcher::new(vec![Err(status_err(403)), Err(status_err(401))]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );
        // One poll per tick (MIN_POLL_GAP throttle), like the oauth twin.
        poller.tick(now()).await; // g → 403
        poller.tick(at(NOW_SECS + 11)).await; // h → 401
        let snapshot = pool.snapshot();
        let g = snapshot.accounts.iter().find(|x| x.id == id("g")).unwrap();
        let h = snapshot.accounts.iter().find(|x| x.id == id("h")).unwrap();
        assert!(g.healthy, "403 semantics unverified → health unchanged");
        assert!(h.healthy, "401 = expired token → refresh path owns it");
        assert_eq!(
            poller.schedule[&id("g")].consecutive_failures,
            1,
            "403 climbs the backoff ladder like any other poll failure"
        );
        assert_eq!(
            poller.schedule[&id("h")].consecutive_failures,
            1,
            "401 climbs the backoff ladder like any other poll failure"
        );
        for account in [g, h] {
            assert!(
                (account.seven_day.expect("7d gauge retained").utilization - 0.65).abs() < 1e-9,
                "a failed billing read never clears the last reading"
            );
        }
    }

    /// A non-status billing failure (malformed body, refused upstream) is a
    /// plain poll failure: it climbs the backoff ladder and never benches.
    #[tokio::test]
    async fn malformed_billing_body_is_a_poll_failure_not_an_auth_failure() {
        let pool = AccountPool::new(&[grok_account("g")]);
        let fetcher = MockFetcher::new(vec![Err(UsageError::from(
            crate::auth::grok_usage::GrokUsageError::Malformed,
        ))]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            GROK_UPSTREAM.into(),
            GROK_UPSTREAM.into(),
            config(),
        );

        poller.tick(now()).await;

        assert_eq!(poller.schedule[&id("g")].consecutive_failures, 1);
        let account = pool.snapshot().accounts[0].clone();
        assert!(account.healthy, "a bad body is not an auth verdict");
        assert!(account.seven_day.is_none(), "unknown is never zero");
    }

    // ---- gemini quota source (docs/gemini/spec.md §R6) ----

    /// A gemini account WITH a resolved project is read through the Cloud Code
    /// quota catalog — with the configured upstream and user agent, the
    /// account's own token and its project — and the default model's quota
    /// lands on `seven_day`.
    #[tokio::test]
    async fn gemini_accounts_are_polled_via_the_quota_catalog() {
        let pool = AccountPool::new(&[gemini_account("m", "proj-m")]);
        let fetcher = MockFetcher::new(vec![Ok(grok_weekly(0.4))]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        )
        .with_gemini(GEMINI_UPSTREAM.into(), GEMINI_USER_AGENT.into());

        poller.tick(now()).await;

        assert_eq!(
            fetcher.call_count(),
            0,
            "a gemini account never hits the anthropic usage endpoint"
        );
        assert!(fetcher.grok_calls.lock().unwrap().is_empty());
        assert_eq!(
            fetcher.gemini_calls.lock().unwrap().clone(),
            vec![(
                GEMINI_UPSTREAM.to_string(),
                GEMINI_USER_AGENT.to_string(),
                "at-m".to_string(),
                "proj-m".to_string()
            )],
            "quota read carries the configured upstream + user agent, the token and the project"
        );
        let account = pool.snapshot().accounts[0].clone();
        assert!(
            (account.seven_day.expect("7d gauge").utilization - 0.4).abs() < 1e-9,
            "the default model's quota fills the 7d gauge"
        );
        assert_eq!(poller.schedule[&id("m")].consecutive_failures, 0);
    }

    /// Without `with_gemini` the poller reads from the `GeminiConfig` defaults,
    /// so a construction site that never wires the config still polls the
    /// documented host rather than nothing.
    #[tokio::test]
    async fn gemini_quota_read_defaults_to_the_gemini_config_defaults() {
        let pool = AccountPool::new(&[gemini_account("m", "proj-m")]);
        let fetcher = MockFetcher::new(vec![Ok(grok_weekly(0.4))]);
        let mut poller = UsagePoller::with_fetcher(
            pool,
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );

        poller.tick(now()).await;

        let defaults = GeminiConfig::default();
        let calls = fetcher.gemini_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, defaults.upstream);
        assert_eq!(calls[0].1, defaults.user_agent);
    }

    /// An offline-imported gemini account has no project until its first
    /// request resolves one: there is no usage source yet. The poll is a
    /// SKIP — no call, no fabricated window, no failure on the ladder.
    #[tokio::test]
    async fn gemini_account_without_a_project_is_skipped_not_failed() {
        let pool = AccountPool::new(&[gemini_account("m", "")]);
        let fetcher = MockFetcher::new(vec![Ok(grok_weekly(0.4))]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        )
        .with_gemini(GEMINI_UPSTREAM.into(), GEMINI_USER_AGENT.into());

        poller.tick(now()).await;

        assert_eq!(fetcher.call_count(), 0);
        assert!(fetcher.grok_calls.lock().unwrap().is_empty());
        assert!(
            fetcher.gemini_calls.lock().unwrap().is_empty(),
            "no project → no quota read"
        );
        assert_eq!(
            poller.schedule[&id("m")].consecutive_failures,
            0,
            "a missing usage source is not a poll failure"
        );
        let account = pool.snapshot().accounts[0].clone();
        assert!(account.seven_day.is_none(), "unknown is never zero");
        assert!(account.five_hour.is_none(), "unknown is never zero");
        assert!(account.healthy);
    }

    /// Self-cleaning unique temp dir (no tempfile dev-dependency).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "llmux-usage-poller-test-{}-{}",
                std::process::id(),
                ulid::Ulid::new()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// [`gemini_account`] whose access token is valid for another WALL-CLOCK
    /// hour (token expiry is wall-clock; the poller's `now` is synthetic).
    fn live_gemini_account(name: &str) -> AccountConfig {
        let mut account = gemini_account(name, "");
        if let AccountCredential::Gemini { expires_at_ms, .. } = &mut account.credential {
            *expires_at_ms = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or(0)
                + 3_600_000;
        }
        account
    }

    /// A Cloud Code stand-in for the project lookup: every request is
    /// answered `(status, reply)` and its `authorization` recorded.
    async fn spawn_lookup_mock(
        status: u16,
        reply: &'static str,
    ) -> (String, std::sync::Arc<Mutex<Vec<Option<String>>>>) {
        let seen: std::sync::Arc<Mutex<Vec<Option<String>>>> = std::sync::Arc::default();
        let recorder = seen.clone();
        let app = axum::Router::new().fallback(move |headers: http::HeaderMap| {
            let recorder = recorder.clone();
            async move {
                recorder.lock().unwrap().push(
                    headers
                        .get(http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string),
                );
                (http::StatusCode::from_u16(status).unwrap(), reply)
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://127.0.0.1:{}", addr.port()), seen)
    }

    /// The pool + a config file seeded with the same live, project-less
    /// gemini account `m`.
    fn persisted_live_gemini(dir: &TempDir) -> (AccountPool, PathBuf) {
        let accounts = vec![live_gemini_account("m")];
        let path = dir.0.join("llmux.json");
        let config = crate::config::Config {
            accounts: accounts.clone(),
            ..Default::default()
        };
        crate::config::save_path(&path, &config).expect("seed config");
        (AccountPool::new(&accounts), path)
    }

    fn project_on_disk(path: &std::path::Path) -> String {
        let config = crate::config::load_path(path).expect("config");
        match &config.accounts[0].credential {
            AccountCredential::Gemini { project_id, .. } => project_id.clone(),
            other => panic!("expected a gemini credential, got {other:?}"),
        }
    }

    /// An imported account whose project was never resolved (the operator
    /// only watches usage): the poll resolves it ONCE with the account's own
    /// token, applies + persists it, and reads the quota on it — the reading
    /// lands (it carries the post-write fingerprint). The next poll reads the
    /// quota straight away.
    #[tokio::test]
    async fn gemini_account_without_a_project_resolves_it_then_reads_quota() {
        let (upstream, lookups) =
            spawn_lookup_mock(200, r#"{"cloudaicompanionProject":"proj-lazy"}"#).await;
        let dir = TempDir::new();
        let (pool, path) = persisted_live_gemini(&dir);
        let fetcher = MockFetcher::new(vec![Ok(grok_weekly(0.4)), Ok(grok_weekly(0.5))]);
        let hub = std::sync::Arc::new(crate::dashboard::DashboardHub::default());
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        )
        .with_gemini(upstream.clone(), GEMINI_USER_AGENT.into())
        .with_gemini_projects(reqwest::Client::new(), Some(path.clone()))
        .with_hub(hub.clone());

        poller.tick(now()).await;
        let history = hub.account_history.snapshot();
        assert_eq!(history["m"][0].outcome, "success");
        assert_eq!(history["m"][0].http_status, Some(200));

        assert_eq!(
            lookups.lock().unwrap().clone(),
            vec![Some("Bearer at-m".to_string())],
            "one lookup, with the account's token"
        );
        assert_eq!(
            fetcher.gemini_calls.lock().unwrap().clone(),
            vec![(
                upstream.clone(),
                GEMINI_USER_AGENT.to_string(),
                "at-m".to_string(),
                "proj-lazy".to_string()
            )],
            "the quota read addresses the resolved project"
        );
        let account = pool.snapshot().accounts[0].clone();
        assert!(
            (account.seven_day.expect("7d gauge").utilization - 0.4).abs() < 1e-9,
            "the reading landed on the resolved credential"
        );
        assert_eq!(poller.schedule[&id("m")].consecutive_failures, 0);
        assert!(matches!(
            pool.credential(&id("m")),
            Some(AccountCredential::Gemini { project_id, .. }) if project_id == "proj-lazy"
        ));
        assert_eq!(project_on_disk(&path), "proj-lazy", "persisted");

        poller.tick(at(NOW_SECS + 400)).await;
        assert_eq!(lookups.lock().unwrap().len(), 1, "resolved once");
        assert_eq!(fetcher.gemini_calls.lock().unwrap().len(), 2);
    }

    /// The startup pass holds the daemon's readiness, so it never looks a
    /// project up: the project-less account is skipped there exactly as
    /// before (no lookup, no quota read, no failure), and its next scheduled
    /// poll resolves the project and reads the quota.
    #[tokio::test]
    async fn gemini_project_lookup_waits_for_a_scheduled_poll_never_prime() {
        let (upstream, lookups) =
            spawn_lookup_mock(200, r#"{"cloudaicompanionProject":"proj-lazy"}"#).await;
        let dir = TempDir::new();
        let (pool, path) = persisted_live_gemini(&dir);
        let fetcher = MockFetcher::new(vec![Ok(grok_weekly(0.4))]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        )
        .with_gemini(upstream.clone(), GEMINI_USER_AGENT.into())
        .with_gemini_projects(reqwest::Client::new(), Some(path.clone()));

        poller.prime(now()).await;

        assert!(lookups.lock().unwrap().is_empty(), "prime never looks up");
        assert!(fetcher.gemini_calls.lock().unwrap().is_empty());
        assert_eq!(poller.schedule[&id("m")].consecutive_failures, 0);
        assert!(pool.snapshot().accounts[0].seven_day.is_none());
        assert_eq!(project_on_disk(&path), "");

        poller.tick(at(NOW_SECS + 400)).await;

        assert_eq!(
            lookups.lock().unwrap().len(),
            1,
            "the scheduled poll looks up"
        );
        assert_eq!(
            fetcher.gemini_calls.lock().unwrap().clone(),
            vec![(
                upstream,
                GEMINI_USER_AGENT.to_string(),
                "at-m".to_string(),
                "proj-lazy".to_string()
            )]
        );
        assert!(pool.snapshot().accounts[0].seven_day.is_some());
        assert_eq!(project_on_disk(&path), "proj-lazy");
    }

    /// The lookup failing keeps today's skip: no quota read, no window, no
    /// failure on the ladder (the next attempt is the next scheduled poll,
    /// never a tight retry), no bench, nothing persisted.
    #[tokio::test]
    async fn gemini_project_lookup_failure_on_the_poll_is_a_skip() {
        let (upstream, lookups) = spawn_lookup_mock(403, r#"{"error":{"code":403}}"#).await;
        let dir = TempDir::new();
        let (pool, path) = persisted_live_gemini(&dir);
        let fetcher = MockFetcher::new(vec![Ok(grok_weekly(0.4))]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        )
        .with_gemini(upstream, GEMINI_USER_AGENT.into())
        .with_gemini_projects(reqwest::Client::new(), Some(path.clone()));

        poller.tick(now()).await;
        // Inside the poll interval: the account is not due, no second lookup.
        poller.tick(at(NOW_SECS + 60)).await;

        assert_eq!(lookups.lock().unwrap().len(), 1, "once per poll");
        assert!(fetcher.gemini_calls.lock().unwrap().is_empty());
        assert_eq!(poller.schedule[&id("m")].consecutive_failures, 0);
        let account = pool.snapshot().accounts[0].clone();
        assert!(account.seven_day.is_none(), "unknown is never zero");
        assert!(account.healthy, "never benched");
        assert_eq!(project_on_disk(&path), "");
    }

    /// No gemini quota status benches (spec §R6): a 403 from the quota read is
    /// a plain poll failure that climbs the ladder and keeps the last reading.
    /// The ToS-suspension 403 is a REQUEST-path verdict, not this read's.
    #[tokio::test]
    async fn gemini_quota_forbidden_is_a_poll_failure_not_an_auth_failure() {
        let pool = AccountPool::new(&[gemini_account("m", "proj-m")]);
        pool.record_usage(&id("m"), &grok_weekly(0.4), now());
        let fetcher = MockFetcher::new(vec![Err(UsageError::from(GeminiUsageError::Status {
            status: http::StatusCode::FORBIDDEN,
        }))]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        )
        .with_gemini(GEMINI_UPSTREAM.into(), GEMINI_USER_AGENT.into());

        poller.tick(now()).await;

        assert_eq!(fetcher.gemini_calls.lock().unwrap().len(), 1);
        let account = pool.snapshot().accounts[0].clone();
        assert!(account.healthy, "a quota read never benches");
        assert_eq!(account.auth_failure_status, None);
        assert_eq!(
            poller.schedule[&id("m")].consecutive_failures,
            1,
            "403 climbs the backoff ladder like any other poll failure"
        );
        assert!(
            (account.seven_day.expect("7d gauge retained").utilization - 0.4).abs() < 1e-9,
            "a failed quota read never clears the last reading"
        );
    }

    /// A status lifts into the shared status-shaped failure; every other gemini
    /// failure keeps only the module's sanitized phrase.
    #[test]
    fn gemini_usage_errors_lift_like_grok_billing_errors() {
        let status = UsageError::from(GeminiUsageError::Status {
            status: http::StatusCode::UNAUTHORIZED,
        });
        assert!(matches!(
            status,
            UsageError::Status { status } if status == http::StatusCode::UNAUTHORIZED
        ));
        let malformed = UsageError::from(GeminiUsageError::Malformed);
        assert!(matches!(malformed, UsageError::GeminiQuota(_)));
        assert_eq!(
            malformed.to_string(),
            format!(
                "gemini quota read failed: {}",
                GeminiUsageError::Malformed.sanitized()
            )
        );
    }

    #[tokio::test]
    async fn removed_accounts_drop_their_schedule() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        let fetcher = MockFetcher::new(vec![Ok(snapshot_with(0.1))]);
        let mut poller = UsagePoller::with_fetcher(
            pool.clone(),
            &fetcher,
            "http://x".into(),
            GROK_UPSTREAM.into(),
            config(),
        );
        poller.tick(now()).await;
        assert!(poller.schedule.contains_key(&id("a")));
        pool.reload_accounts(&[]);
        poller.tick(at(NOW_SECS + 1)).await;
        assert!(poller.schedule.is_empty());
    }
}
