//! AccountPool: owns per-account scheduler state and applies events.
//!
//! Concurrency model (see `.prd/02-architecture.md`): `PoolState` lives behind
//! `Arc<std::sync::RwLock<_>>` — every mutation is sync and IO-free, so a std
//! lock is correct (short critical sections, no `.await` while held) and lets
//! `AccountLease::drop` release synchronously. Decisions are NOT made here:
//! `select::pick` is a pure function over a `PoolSnapshot`.

pub mod headers;
pub mod idle_probe;
pub mod select;
pub mod usage;
pub mod window;

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use crate::config::{AccountConfig, AccountCredential};
use crate::routing::BackendGroup;
use headers::{ParsedRateLimitHeaders, WindowReading};
use usage::{ScopedLimitReading, UsageSnapshot};
use window::{QuotaWindow, ScopedQuotaWindow, WindowSource};

/// The group slot used for the legacy (routing-disabled) selection path:
/// when callers pass `group = None`, a SINGLE shared current slot is used so
/// behavior is byte-for-byte unchanged. `Claude` is that slot. Routing-on
/// callers always pass `Some(group)`, so the legacy slot never coexists with
/// real per-group slots in one process.
const LEGACY_GROUP: BackendGroup = BackendGroup::Claude;

/// Heuristic cooldown applied to a 429 WITHOUT `retry-after`. Such a 429 is
/// almost always a transient, server-side limit (Anthropic "Server is
/// temporarily limiting requests (not your usage limit)") rather than the
/// account's own quota — a real quota 429 carries `retry-after`/reset headers,
/// and the 5h/7d usage windows are the authoritative quota gate anyway. So this
/// is SHORT (recover fast, let the client retry) and self-heals early on fresh
/// data showing capacity. A 60-minute park here would strand a fully-usable
/// account (≈2% utilized) for an hour on a momentary blip.
///
/// 8s, not 30s: a retry-after-less 429 is a per-minute-window blip, so 30s
/// over-parks. Paired with heuristic-degraded selection
/// (`select::heuristic_degraded_mode`), which serves the soonest-freed account
/// when an in-flight burst parks the whole group this way — so even this short
/// park no longer hard-locks the pool.
pub const DEFAULT_HEURISTIC_COOLDOWN: Duration = Duration::from_secs(8);

/// Utilization at/above which a window is treated as "critical" for Fable
/// scope decisions (fable-usage W2): the preemptive Fable-routing exclusion
/// (`AccountSnapshot::fable_weekly_exhausted`) and the account-wide escalation
/// of a Fable 429 (`AccountState::account_wide_critical`). Matches the ≥95%
/// bar the W0 design fixed for Fable avoidance.
pub const CRITICAL_UTILIZATION: f64 = 0.95;

/// Stable account identifier — the config `name`. Newtype so ids don't get
/// mixed up with credentials or display strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountId(pub String);

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Auth-level health, distinct from quota state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountHealth {
    Healthy,
    /// Refresh failed or upstream said 401 twice — needs re-login.
    AuthFailed,
    /// Persistent non-auth error; message kept for status output.
    Errored(String),
}

/// Why an account is cooling down — fresh usage data may self-heal a
/// `Heuristic` guess but must not override an explicit `RetryAfter` park.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CooldownSource {
    /// Upstream 429 carried `retry-after`; park exactly that long.
    RetryAfter,
    /// Heuristic cooldown (no retry-after); clearable by fresh capacity.
    Heuristic,
}

/// The scope a cooldown applies to (fable-usage W2). The cooldown KEY is
/// `Account × CooldownScope`: an account can hold an [`Self::AccountWide`]
/// cooldown AND one-or-more [`Self::ModelScoped`] cooldowns at the same time,
/// independently. `AccountWide` is realized by the account's existing
/// `cooldown_until`/`cooldown_source` fields (unchanged behavior); `ModelScoped`
/// entries live in [`AccountState::scoped_cooldowns`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CooldownScope {
    /// The whole account is parked (RetryAfter park, whole-account exhaustion,
    /// or a scope-blind / non-Fable 429).
    AccountWide,
    /// Only requests for a given model scope are parked (the String is the
    /// scope label, e.g. "Fable"), matched case-insensitively via
    /// [`Self::matches_label`]. Other scopes on the same account stay eligible.
    ModelScoped(String),
}

impl CooldownScope {
    /// The scope label for a [`Self::ModelScoped`], else `None`.
    pub fn label(&self) -> Option<&str> {
        match self {
            CooldownScope::AccountWide => None,
            CooldownScope::ModelScoped(label) => Some(label),
        }
    }

    /// Whether this scope is the model scope `label` (case-insensitive). The
    /// CENTRAL matcher — callers never compare scope strings by hand.
    pub fn matches_label(&self, label: &str) -> bool {
        matches!(self, CooldownScope::ModelScoped(l) if l.eq_ignore_ascii_case(label))
    }
}

/// Classification of a 429 that drives cooldown scoping (fable-usage W2). A
/// scope-blind 429 carries no model info, so this records WHY a given scope was
/// chosen — the guard both strategists demanded against collapsing scope back
/// into a single bool. Kept on the recorded [`ModelScopedCooldown`] and returned
/// by [`PoolState::record_429_classified`] for logging / status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cooldown429Reason {
    /// `retry-after` header present → account-wide RetryAfter park (unchanged).
    HeaderRetryAfter,
    /// Fable-family request 429'd AND the account's `fable_weekly` snapshot
    /// corroborates (is_active / critical / ≥95%). Fable-scoped cooldown set.
    FableObservedCritical,
    /// Fable-family request 429'd but the snapshot shows Fable healthy — the
    /// observed anomaly (`.prd/13` §2c: Fable 429 while snapshot says Fable OK).
    /// Fable-scoped cooldown set ANYWAY (same-account non-Fable 200 proves the
    /// account isn't dead); flagged for a usage-poll refresh. Non-Fable stays
    /// eligible. Never escalated to account-wide on the mismatch alone.
    FableSuspectSnapshotMismatch,
    /// Non-Fable (scope-blind) 429 → account-wide heuristic park. A non-Fable
    /// request failing IS whole-account corroboration (unchanged behavior).
    AccountAllObserved,
    /// The account vanished between selection and recording — nothing applied.
    Unknown,
}

/// One model-scoped cooldown entry on an account (fable-usage W2) — the
/// `ModelScoped` half of the `Account × CooldownScope` key. Fable-scoped
/// cooldowns are always heuristic in nature (a `retry-after` 429 parks
/// account-wide instead), so no `CooldownSource` is stored; the `reason`
/// records the classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelScopedCooldown {
    /// The scope this cooldown parks (always [`CooldownScope::ModelScoped`]).
    pub scope: CooldownScope,
    /// When the scoped park expires.
    pub until: SystemTime,
    /// When it was set (parity with the account-wide `cooldown_set_at`).
    pub set_at: SystemTime,
    /// Why this scoped park was chosen — the scope-provenance guard.
    pub reason: Cooldown429Reason,
}

/// Full per-account scheduler state.
#[derive(Debug, Clone)]
pub struct AccountState {
    pub id: AccountId,
    pub credential: AccountCredential,
    pub health: AccountHealth,
    pub five_hour: Option<QuotaWindow>,
    pub seven_day: Option<QuotaWindow>,
    /// Model-scoped weekly limits from the usage poll's `limits[]` (e.g. the
    /// "Fable" weekly gauge) — generic, keyed by scope label; empty until a
    /// poll reports one. Entries persist until overwritten by fresher data;
    /// an entry whose window has reset reads as unconstrained via
    /// [`QuotaWindow::effective_utilization`], same as the account windows.
    pub scoped_limits: Vec<ScopedQuotaWindow>,
    /// Model-scoped cooldowns (fable-usage W2), keyed by scope label — the
    /// `ModelScoped` half of `Account × CooldownScope`. Independent of the
    /// account-wide `cooldown_until` below: a Fable-scoped park here benches
    /// ONLY Fable requests while the account keeps serving non-Fable traffic.
    pub scoped_cooldowns: Vec<ModelScopedCooldown>,
    pub cooldown_until: Option<SystemTime>,
    pub cooldown_source: Option<CooldownSource>,
    /// When the active cooldown was set. Self-healing requires evidence
    /// STRICTLY NEWER than this — otherwise the same response that carried
    /// the 429 could immediately clear its own cooldown via its headers.
    pub cooldown_set_at: Option<SystemTime>,
    /// Live leases (in-flight requests pinned to this account).
    pub in_flight: u32,
    /// Operator pause (config `paused_accounts`): excluded from automatic
    /// selection AND manual switch until resumed. Windows keep polling so the
    /// gauges stay truthful while parked.
    pub paused: bool,
    /// Per-account ceiling overrides (config `account_limits`); empty = the
    /// global scheduler ceilings apply.
    pub limits: crate::config::AccountLimits,
}

impl AccountState {
    fn fresh(config: &AccountConfig) -> Self {
        Self {
            id: AccountId(config.name.clone()),
            credential: config.credential.clone(),
            health: AccountHealth::Healthy,
            five_hour: None,
            seven_day: None,
            scoped_limits: Vec::new(),
            scoped_cooldowns: Vec::new(),
            cooldown_until: None,
            cooldown_source: None,
            cooldown_set_at: None,
            in_flight: 0,
            paused: false,
            limits: crate::config::AccountLimits::default(),
        }
    }

    /// Merge one window observation, freshest `fetched_at` wins.
    fn merge_window(
        slot: &mut Option<QuotaWindow>,
        reading: WindowReading,
        fetched_at: SystemTime,
        source: WindowSource,
    ) -> bool {
        let keep_existing = slot.is_some_and(|old| old.fetched_at > fetched_at);
        if keep_existing {
            return false;
        }
        *slot = Some(QuotaWindow {
            utilization: reading.utilization,
            resets_at: reading.resets_at,
            fetched_at,
            source,
        });
        true
    }

    /// Merge one scoped-limit observation into the account's scoped list,
    /// keyed by scope label (case-insensitive), freshest `fetched_at` wins —
    /// same policy as [`Self::merge_window`].
    fn merge_scoped(
        slots: &mut Vec<ScopedQuotaWindow>,
        reading: &ScopedLimitReading,
        fetched_at: SystemTime,
        source: WindowSource,
    ) -> bool {
        let merged = ScopedQuotaWindow {
            scope_label: reading.scope_label.clone(),
            window: QuotaWindow {
                utilization: reading.reading.utilization,
                resets_at: reading.reading.resets_at,
                fetched_at,
                source,
            },
            severity: reading.severity,
            is_active: reading.is_active,
        };
        match slots
            .iter_mut()
            .find(|s| s.scope_label.eq_ignore_ascii_case(&reading.scope_label))
        {
            Some(existing) => {
                if existing.window.fetched_at > fetched_at {
                    return false;
                }
                *existing = merged;
            }
            None => slots.push(merged),
        }
        true
    }

    /// Cooldown self-healing: fresh data (strictly newer than the cooldown)
    /// showing capacity (< 100% on every present window) clears a `Heuristic`
    /// cooldown. `RetryAfter` parks are explicit upstream instructions and
    /// are never cleared early.
    fn maybe_self_heal(&mut self, now: SystemTime) {
        if self.cooldown_source != Some(CooldownSource::Heuristic) {
            return;
        }
        let newer_than_cooldown = self.cooldown_set_at.is_none_or(|set_at| now > set_at);
        if !newer_than_cooldown {
            return;
        }
        let windows: Vec<&QuotaWindow> = [&self.five_hour, &self.seven_day]
            .into_iter()
            .flatten()
            .collect();
        let shows_capacity =
            !windows.is_empty() && windows.iter().all(|w| w.effective_utilization(now) < 1.0);
        if shows_capacity {
            self.cooldown_until = None;
            self.cooldown_source = None;
            self.cooldown_set_at = None;
        }
    }

    /// Set (or refresh, keyed by scope label) one model-scoped cooldown
    /// (fable-usage W2). Never touches the account-wide cooldown — that is a
    /// separate, explicit step — so recording a Fable-scoped park here leaves
    /// non-Fable traffic eligible. A `None` `until` (clock overflow) or an
    /// `AccountWide` scope is a no-op.
    fn set_scoped_cooldown(
        &mut self,
        scope: CooldownScope,
        until: Option<SystemTime>,
        set_at: SystemTime,
        reason: Cooldown429Reason,
    ) {
        let (Some(until), Some(label)) = (until, scope.label().map(str::to_string)) else {
            return;
        };
        let entry = ModelScopedCooldown {
            scope,
            until,
            set_at,
            reason,
        };
        match self
            .scoped_cooldowns
            .iter_mut()
            .find(|c| c.scope.matches_label(&label))
        {
            Some(existing) => *existing = entry,
            None => self.scoped_cooldowns.push(entry),
        }
    }

    /// Whether this account's Fable weekly bucket is currently constraining —
    /// the corroboration test for classifying a Fable 429 (observed-critical vs
    /// suspect-snapshot-mismatch). Reset-aware.
    fn fable_weekly_constraining(&self, now: SystemTime) -> bool {
        self.scoped_limits
            .iter()
            .find(|s| {
                s.scope_label
                    .eq_ignore_ascii_case(crate::routing::FABLE_SCOPE_LABEL)
            })
            .is_some_and(|s| s.is_constraining(now, CRITICAL_UTILIZATION))
    }

    /// Whether the account-wide (5h/7d) windows THEMSELVES read critical
    /// (≥ [`CRITICAL_UTILIZATION`], reset-aware) — the only corroboration that
    /// escalates a Fable 429 to an account-wide park.
    fn account_wide_critical(&self, now: SystemTime) -> bool {
        [self.five_hour, self.seven_day]
            .into_iter()
            .flatten()
            .any(|w| w.effective_utilization(now) >= CRITICAL_UTILIZATION)
    }
}

/// The pool's mutable state. All mutations re-validate preconditions before
/// applying (CAS pattern) — see `commit_switch`.
#[derive(Debug, Clone, Default)]
pub struct PoolState {
    pub accounts: Vec<AccountState>,
    /// Currently selected account PER backend group (session stickiness,
    /// independent per group). A group is absent until its first selection
    /// and removed when its accounts are all exhausted. With routing
    /// disabled only the [`LEGACY_GROUP`] slot is ever populated, so the map
    /// degenerates to the single-current-slot behavior of before.
    ///
    /// This is the NonFable / representative slot: every display, status, and
    /// legacy reader keys off it, and it is what a non-Fable request pins.
    pub current: BTreeMap<BackendGroup, AccountId>,
    /// Separate sticky current PER backend group for [`select::RequestScope::
    /// Fable`] requests (fable-usage: fable-head isolation). A Fable request
    /// pins and moves ONLY this slot, so its account rotation never disturbs
    /// the non-Fable `current` above (and vice versa). Same lifecycle: a group
    /// is absent until its first Fable selection, removed when Fable is
    /// exhausted for that group. Draws from the same account pool as `current`
    /// — this isolates stickiness, not inventory.
    pub fable_current: BTreeMap<BackendGroup, AccountId>,
    /// Manual operator pin PER backend group (issue #122): a manual switch is
    /// an informed operator decision, so for [`MANUAL_PIN_DURATION`] the
    /// selector keeps EVERY request of the group — Fable lane included — on
    /// the chosen account. Thresholds, staleness and preemptive Fable
    /// exclusion do not break the pin; only a real failure does (auth
    /// failure, a recorded 429 park, operator pause) or expiry. Cleared on
    /// the pinned account's 429.
    pub manual_pin: BTreeMap<BackendGroup, ManualPin>,
}

/// One manual operator pin (issue #122): the chosen account and when the
/// pin lapses.
#[derive(Debug, Clone, PartialEq)]
pub struct ManualPin {
    pub account: AccountId,
    pub until: SystemTime,
}

/// How long a manual switch pins its group (issue #122): long enough that
/// the operator's deliberate choice survives the evaluation ticks, short
/// enough that automatic quota scheduling resumes on its own.
pub const MANUAL_PIN_DURATION: Duration = Duration::from_secs(300);

/// Switch failed; nothing was mutated.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SwitchError {
    /// CAS failure: `current` is no longer what the caller observed.
    #[error("current account changed (now {actual:?})")]
    CurrentChanged { actual: Option<AccountId> },
    #[error("unknown account {0}")]
    UnknownAccount(AccountId),
    /// Target failed re-validation at commit time.
    #[error("target {account} ineligible: {reason:?}")]
    TargetIneligible {
        account: AccountId,
        reason: select::IneligibleReason,
    },
}

impl PoolState {
    /// Build initial state from config; all accounts healthy with no windows
    /// (cold account = immediately eligible).
    pub fn from_accounts(accounts: &[AccountConfig]) -> Self {
        Self {
            accounts: accounts.iter().map(AccountState::fresh).collect(),
            current: BTreeMap::new(),
            fable_current: BTreeMap::new(),
            manual_pin: BTreeMap::new(),
        }
    }

    fn account_mut(&mut self, account: &AccountId) -> Option<&mut AccountState> {
        self.accounts.iter_mut().find(|a| &a.id == account)
    }

    /// Record rate-limit headers from an upstream response. Freshest
    /// `fetched_at` wins per window; header data also self-heals a
    /// `Heuristic` cooldown when it shows capacity. When no unified windows
    /// are present, the most-constrained standard (API-key) bucket is
    /// recorded into the 5h slot so API-key accounts get proactive
    /// scheduling too (the token bucket's short reset horizon means the
    /// reading expires quickly and degrades back to cold). grok's
    /// cli-chat-proxy buckets carry no reset, so a reset-less standard
    /// reading gets an estimated `STANDARD_RESET_FALLBACK` horizon — but ONLY
    /// for Grok accounts, since the `x-ratelimit-*` names are provider-generic
    /// and other groups must keep the strict both-fields-required behavior.
    pub fn record_headers(
        &mut self,
        account: &AccountId,
        parsed: &ParsedRateLimitHeaders,
        now: SystemTime,
    ) {
        let Some(acct) = self.account_mut(account) else {
            return;
        };
        let mut recorded = false;
        if let Some(reading) = parsed.five_hour {
            recorded |= AccountState::merge_window(
                &mut acct.five_hour,
                reading,
                now,
                WindowSource::Headers,
            );
        }
        if let Some(reading) = parsed.seven_day {
            recorded |= AccountState::merge_window(
                &mut acct.seven_day,
                reading,
                now,
                WindowSource::Headers,
            );
        }
        if let Some(reading) = parsed.fable_weekly {
            // The `7d_oi` scoped bucket rides every response (issue #123), so
            // the Fable gauge stays live between usage polls and a COLD gauge
            // heals on the first request through the account. Severity is
            // derived for DISPLAY only (`is_constraining` keys on utilization,
            // never on these labels); the next usage poll's freshest-wins
            // merge restores upstream's own labels.
            let severity = if reading.utilization >= 0.95 {
                window::LimitSeverity::Critical
            } else if reading.utilization >= 0.80 {
                window::LimitSeverity::Warning
            } else {
                window::LimitSeverity::Normal
            };
            recorded |= AccountState::merge_scoped(
                &mut acct.scoped_limits,
                &usage::ScopedLimitReading {
                    scope_label: crate::routing::FABLE_SCOPE_LABEL.to_string(),
                    reading,
                    severity,
                    is_active: false,
                },
                now,
                WindowSource::Headers,
            );
        }
        if parsed.five_hour.is_none() && parsed.seven_day.is_none() {
            // The `x-ratelimit-*` fallback names are provider-generic, so a
            // codex/anthropic response carrying them without a reset would
            // otherwise get a synthetic-horizon window. Gate the reset-less
            // fallback to Grok accounts (grok's cli-chat-proxy sends no
            // reset); every other group keeps the strict both-fields-required
            // behavior — a reset-less reading is dropped.
            let is_grok = BackendGroup::from_kind(acct.credential.kind()) == BackendGroup::Grok;
            let reading = parsed.standard.and_then(|s| {
                if is_grok {
                    s.as_window_reading_with_fallback_reset(now + headers::STANDARD_RESET_FALLBACK)
                } else {
                    s.as_window_reading()
                }
            });
            if let Some(reading) = reading {
                recorded |= AccountState::merge_window(
                    &mut acct.five_hour,
                    reading,
                    now,
                    WindowSource::Headers,
                );
            }
        }
        if recorded {
            acct.maybe_self_heal(now);
        }
    }

    /// Record a `/api/oauth/usage` poll result. Same freshness merge as
    /// headers; fresh data showing capacity clears `Heuristic` cooldowns.
    /// Scoped (`limits[]`) readings merge into the account's scoped list but
    /// deliberately do NOT feed the self-heal gate: cooldown behavior stays
    /// keyed to the account-wide windows only (scope-aware cooldown is W2).
    pub fn record_usage(&mut self, account: &AccountId, usage: &UsageSnapshot, now: SystemTime) {
        let Some(acct) = self.account_mut(account) else {
            return;
        };
        let mut recorded = false;
        if let Some(reading) = usage.five_hour {
            recorded |= AccountState::merge_window(
                &mut acct.five_hour,
                reading,
                now,
                WindowSource::UsagePoll,
            );
        }
        if let Some(reading) = usage.seven_day {
            recorded |= AccountState::merge_window(
                &mut acct.seven_day,
                reading,
                now,
                WindowSource::UsagePoll,
            );
        }
        for reading in &usage.scoped {
            AccountState::merge_scoped(
                &mut acct.scoped_limits,
                reading,
                now,
                WindowSource::UsagePoll,
            );
        }
        if recorded {
            acct.maybe_self_heal(now);
        }
    }

    /// Force every account's usage state back to COLD (issue #115): providers
    /// occasionally reset quota server-side, leaving these in-memory windows
    /// and cooldowns overstating utilization until they age out. Clears the
    /// account windows, the model-scoped limits and every cooldown — and ONLY
    /// usage: health, pause state, per-account ceilings, credentials and
    /// in-flight leases are operator/liveness state and stay untouched.
    /// Gauges repopulate from the next usage poll / response headers (the
    /// idle-probe cold sweep keeps cold accounts refreshing). Returns the
    /// number of accounts reset.
    pub fn reset_usage(&mut self) -> usize {
        for account in &mut self.accounts {
            account.five_hour = None;
            account.seven_day = None;
            account.scoped_limits.clear();
            account.scoped_cooldowns.clear();
            account.cooldown_until = None;
            account.cooldown_source = None;
            account.cooldown_set_at = None;
        }
        self.accounts.len()
    }

    /// Record an upstream 429. With `retry_after` the account parks exactly
    /// that long (`CooldownSource::RetryAfter`); without it a default
    /// heuristic cooldown applies.
    pub fn record_429(
        &mut self,
        account: &AccountId,
        retry_after: Option<Duration>,
        now: SystemTime,
    ) {
        self.clear_manual_pin_for(account);
        let Some(acct) = self.account_mut(account) else {
            return;
        };
        let (duration, source) = match retry_after {
            Some(d) => (d, CooldownSource::RetryAfter),
            None => (DEFAULT_HEURISTIC_COOLDOWN, CooldownSource::Heuristic),
        };
        acct.cooldown_until = now.checked_add(duration);
        acct.cooldown_source = Some(source);
        acct.cooldown_set_at = Some(now);
    }

    /// Record an upstream 429 with SCOPE-AWARE classification (fable-usage W2,
    /// the core U8 requirement). A scope-blind 429 carries no model info, so the
    /// requested `model` decides the cooldown scope:
    ///
    /// - `retry_after` present → account-wide RetryAfter park, exactly as today
    ///   ([`Self::record_429`]).
    /// - Non-Fable request (no retry-after) → account-wide heuristic park, as
    ///   today: a non-Fable failure IS whole-account corroboration.
    /// - **Fable-family request (no retry-after) → a `ModelScoped("Fable")`
    ///   cooldown FIRST, ALWAYS** — even when the snapshot says Fable looks
    ///   healthy (the `.prd/13` §2c anomaly), because a same-account non-Fable
    ///   200 proves the account isn't dead. The account-wide cooldown is left
    ///   untouched, so non-Fable traffic stays eligible. It escalates to an
    ///   account-wide park ONLY when the 5h/7d windows THEMSELVES read critical.
    ///
    /// Returns the [`Cooldown429Reason`] chosen, for logging / status. NB: on
    /// [`Cooldown429Reason::FableSuspectSnapshotMismatch`] the design also wants
    /// an immediate usage-poll refresh; that refresh wiring is deferred (the
    /// recorded reason is the flag) — the load-bearing invariant here is that
    /// non-Fable stays eligible, which holds regardless.
    pub fn record_429_classified(
        &mut self,
        account: &AccountId,
        retry_after: Option<Duration>,
        model: Option<&str>,
        now: SystemTime,
    ) -> Cooldown429Reason {
        // A 429 on a manually pinned account is the real failure that ends
        // the operator pin (issue #122) — every branch below parks something,
        // so clear up front (idempotent).
        self.clear_manual_pin_for(account);
        if retry_after.is_some() {
            self.record_429(account, retry_after, now);
            return Cooldown429Reason::HeaderRetryAfter;
        }
        if !crate::routing::is_fable_model(model) {
            self.record_429(account, None, now);
            return Cooldown429Reason::AccountAllObserved;
        }
        let Some(acct) = self.account_mut(account) else {
            return Cooldown429Reason::Unknown;
        };
        let reason = if acct.fable_weekly_constraining(now) {
            Cooldown429Reason::FableObservedCritical
        } else {
            Cooldown429Reason::FableSuspectSnapshotMismatch
        };
        acct.set_scoped_cooldown(
            CooldownScope::ModelScoped(crate::routing::FABLE_SCOPE_LABEL.to_string()),
            now.checked_add(DEFAULT_HEURISTIC_COOLDOWN),
            now,
            reason,
        );
        // Escalate to an account-wide park ONLY on genuine corroboration: the
        // 5h/7d windows themselves read critical. `acct`'s borrow ends here.
        let escalate = acct.account_wide_critical(now);
        if escalate {
            self.record_429(account, None, now);
        }
        reason
    }

    /// Record an auth failure (second 401 after a forced refresh, or a
    /// failed refresh). Marks the account `AuthFailed` until re-login. An
    /// auth failure on a manually pinned account ends the pin (issue #122).
    pub fn record_auth_failure(&mut self, account: &AccountId) {
        self.clear_manual_pin_for(account);
        if let Some(acct) = self.account_mut(account) {
            acct.health = AccountHealth::AuthFailed;
        }
    }

    /// Replace an account's credential after a successful OAuth refresh or a
    /// config reload; restores `Healthy` if it was `AuthFailed`.
    pub fn update_credential(&mut self, account: &AccountId, credential: AccountCredential) {
        if let Some(acct) = self.account_mut(account) {
            acct.credential = credential;
            if acct.health == AccountHealth::AuthFailed {
                acct.health = AccountHealth::Healthy;
            }
        }
    }

    /// Commit an account switch with compare-and-swap semantics: aborts with
    /// `CurrentChanged` if `current` differs from `expected_current` (another
    /// task already switched) and with `TargetIneligible` if the target
    /// stopped being eligible between selection and commit. Never cancels
    /// in-flight leases — they keep their pinned credential until Drop.
    pub fn commit_switch(
        &mut self,
        group: Option<BackendGroup>,
        expected_current: Option<&AccountId>,
        target: &AccountId,
        params: &select::SelectParams,
        now: SystemTime,
    ) -> Result<(), SwitchError> {
        self.commit_switch_scoped(
            group,
            expected_current,
            target,
            params,
            now,
            select::RequestScope::NonFable,
        )
    }

    /// Scope-aware [`Self::commit_switch`] (fable-usage W2): re-validates the
    /// target through the SAME scope-aware gate the scope-aware selector used,
    /// so a Fable request never commits an account its Fable-scoped cooldown /
    /// preemptive exclusion would refuse. `RequestScope::NonFable` is exactly
    /// the pre-W2 behavior.
    pub fn commit_switch_scoped(
        &mut self,
        group: Option<BackendGroup>,
        expected_current: Option<&AccountId>,
        target: &AccountId,
        params: &select::SelectParams,
        now: SystemTime,
        scope: select::RequestScope,
    ) -> Result<(), SwitchError> {
        let slot = group.unwrap_or(LEGACY_GROUP);
        // CAS against the scope's OWN slot: a Fable commit compares/writes
        // `fable_current`, a non-Fable commit `current` — so the two scopes'
        // sticky currents never clobber each other. `observed`'s borrow ends
        // before `self.snapshot()` below re-borrows `self`.
        let observed = match scope {
            select::RequestScope::Fable => self.fable_current.get(&slot),
            _ => self.current.get(&slot),
        };
        if observed != expected_current {
            return Err(SwitchError::CurrentChanged {
                actual: observed.cloned(),
            });
        }
        let snapshot = self.snapshot();
        let target_snapshot = snapshot
            .accounts
            .iter()
            .find(|a| &a.id == target)
            .ok_or_else(|| SwitchError::UnknownAccount(target.clone()))?;
        let headers_only = select::headers_only_mode(&snapshot, params, group, now);
        let heuristic_degraded = select::heuristic_degraded_mode(&snapshot, params, group, now);
        // The selector may legitimately pick a Heuristic-parked account in
        // heuristic-degraded mode (transient-429 lockout recovery); the commit
        // re-validation must use the SAME gate so it does not reject what
        // `pick` just chose.
        if let Some(reason) = select::gate_scoped(
            target_snapshot,
            params,
            now,
            headers_only,
            heuristic_degraded,
            scope,
        ) {
            // A target under an active manual pin (issue #122) is the
            // operator's explicit choice: only a REAL failure refuses it —
            // thresholds / staleness / the preemptive Fable exclusion were
            // just overridden. Everything else keeps the full gate.
            let pinned = self
                .manual_pin
                .get(&slot)
                .is_some_and(|pin| &pin.account == target && pin.until > now);
            if !pinned || reason.hard_failure() {
                return Err(SwitchError::TargetIneligible {
                    account: target.clone(),
                    reason,
                });
            }
        }
        match scope {
            select::RequestScope::Fable => self.fable_current.insert(slot, target.clone()),
            _ => self.current.insert(slot, target.clone()),
        };
        Ok(())
    }

    /// Immutable snapshot for the pure selector and for `/llmux/status`.
    pub fn snapshot(&self) -> PoolSnapshot {
        PoolSnapshot {
            accounts: self
                .accounts
                .iter()
                .map(|a| AccountSnapshot {
                    id: a.id.clone(),
                    healthy: a.health == AccountHealth::Healthy,
                    credential_kind: a.credential.kind(),
                    group: BackendGroup::from_kind(a.credential.kind()),
                    five_hour: a.five_hour,
                    seven_day: a.seven_day,
                    scoped_limits: a.scoped_limits.clone(),
                    scoped_cooldowns: a.scoped_cooldowns.clone(),
                    cooldown_until: a.cooldown_until,
                    cooldown_source: a.cooldown_source,
                    in_flight: a.in_flight,
                    token_expires_at_ms: match &a.credential {
                        AccountCredential::Oauth { expires_at_ms, .. }
                        | AccountCredential::Codex { expires_at_ms, .. }
                            if *expires_at_ms > 0 =>
                        {
                            Some(*expires_at_ms)
                        }
                        _ => None,
                    },
                    last_refresh_ms: a.credential.last_refresh_ms(),
                    paused: a.paused,
                    limits: a.limits,
                })
                .collect(),
            current: self.current.clone(),
            fable_current: self.fable_current.clone(),
            manual_pin: self.manual_pin.clone(),
        }
    }

    /// Drop a manual pin naming `account` (issue #122): a real failure on
    /// the pinned account — a recorded 429 park or an auth failure — ends
    /// the operator pin immediately; thresholds never call this.
    fn clear_manual_pin_for(&mut self, account: &AccountId) {
        self.manual_pin.retain(|_, pin| &pin.account != account);
    }
}

impl PoolSnapshot {
    /// The current account for one backend group, if any.
    pub fn current_for_group(&self, group: BackendGroup) -> Option<&AccountId> {
        self.current.get(&group)
    }

    /// The manual operator pin for a (possibly legacy / group-less) slot
    /// (issue #122) — same slot resolution as the current-slot readers.
    pub fn manual_pin_for(&self, group: Option<BackendGroup>) -> Option<&ManualPin> {
        self.manual_pin.get(&group.unwrap_or(LEGACY_GROUP))
    }

    /// The current account for the legacy / group-less path (routing
    /// disabled): the [`LEGACY_GROUP`] slot.
    pub fn legacy_current(&self) -> Option<&AccountId> {
        self.current.get(&LEGACY_GROUP)
    }

    /// A single representative current account for scalar status output and
    /// for the many display readers that show "the" active account: the
    /// claude-group slot if present, else the codex-group slot. With routing
    /// disabled this is exactly the legacy current.
    pub fn representative_current(&self) -> Option<&AccountId> {
        self.current
            .get(&BackendGroup::Claude)
            .or_else(|| self.current.get(&BackendGroup::Codex))
    }

    /// Whether `id` is the current account in ANY group, in EITHER scope
    /// (non-Fable or Fable) — the predicate the display layer uses to mark the
    /// active row(s). A Fable-head account is marked active even when it is not
    /// the non-Fable current.
    pub fn is_current(&self, id: &AccountId) -> bool {
        self.current.values().any(|c| c == id) || self.fable_current.values().any(|c| c == id)
    }

    /// The current account for one backend group in a given request scope: the
    /// `fable_current` slot for [`select::RequestScope::Fable`], the non-Fable
    /// `current` slot otherwise. This is the scope-correct read the selector /
    /// commit path key off; the group-only [`Self::current_for_group`] stays
    /// the non-Fable/representative reader for display.
    pub fn current_for_scope(
        &self,
        group: BackendGroup,
        scope: select::RequestScope,
    ) -> Option<&AccountId> {
        match scope {
            select::RequestScope::Fable => self.fable_current.get(&group),
            _ => self.current.get(&group),
        }
    }

    /// Scope-aware [`Self::legacy_current`]: the [`LEGACY_GROUP`] slot in the
    /// scope's own map (routing-disabled path).
    pub fn legacy_current_scoped(&self, scope: select::RequestScope) -> Option<&AccountId> {
        match scope {
            select::RequestScope::Fable => self.fable_current.get(&LEGACY_GROUP),
            _ => self.current.get(&LEGACY_GROUP),
        }
    }
}

/// Read-only projection of one account for selection / status.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountSnapshot {
    pub id: AccountId,
    pub healthy: bool,
    pub credential_kind: &'static str,
    /// Backend group this account belongs to, derived from `credential_kind`
    /// (codex credential → Codex, oauth/apikey → Claude). The selector's
    /// group filter and per-group stickiness key off this.
    pub group: BackendGroup,
    pub five_hour: Option<QuotaWindow>,
    pub seven_day: Option<QuotaWindow>,
    /// Model-scoped weekly limits (`limits[]` weekly_scoped rows from the
    /// usage poll), e.g. the "Fable" weekly gauge. Empty when never seen.
    pub scoped_limits: Vec<ScopedQuotaWindow>,
    /// Active model-scoped cooldowns (fable-usage W2) — the `ModelScoped` half
    /// of `Account × CooldownScope`, projected for the scope-aware selector and
    /// for status/display to show "fable cooldown" distinct from a whole-account
    /// cooldown. Empty when no scoped park is set.
    pub scoped_cooldowns: Vec<ModelScopedCooldown>,
    pub cooldown_until: Option<SystemTime>,
    pub cooldown_source: Option<CooldownSource>,
    pub in_flight: u32,
    /// OAuth access-token expiry (epoch ms) for the dashboard's token-health
    /// column; `None` for API-key accounts and for oauth accounts whose
    /// expiry is unknown (`expires_at_ms == 0`).
    pub token_expires_at_ms: Option<u64>,
    /// When the access token was last successfully refreshed (epoch ms);
    /// `None` for API-key accounts and never-refreshed oauth accounts —
    /// rendered as "never" in the dashboard.
    pub last_refresh_ms: Option<u64>,
    /// Operator pause (config `paused_accounts`): gates automatic selection
    /// and manual switch in every mode until resumed.
    pub paused: bool,
    /// Per-account ceiling overrides (config `account_limits`); `None` fields
    /// fall back to the global [`select::SelectParams`] ceilings.
    pub limits: crate::config::AccountLimits,
}

impl AccountSnapshot {
    /// The "Fable" weekly scoped window, if this account carries one — the
    /// entry in [`Self::scoped_limits`] whose `scope_label` matches "Fable"
    /// case-insensitively. Convenience surface for the dashboard doc /
    /// `/llmux/status` JSON; the full scoped list stays available for other
    /// (future) scoped models without another lookup helper.
    pub fn fable_weekly(&self) -> Option<&ScopedQuotaWindow> {
        self.scoped_limits.iter().find(|s| {
            s.scope_label
                .eq_ignore_ascii_case(crate::routing::FABLE_SCOPE_LABEL)
        })
    }

    /// Whether a model-scoped cooldown for `label` is currently parked
    /// (case-insensitive, wall-clock live). The central scoped-cooldown query.
    pub fn scoped_cooldown_active(&self, label: &str, now: SystemTime) -> bool {
        self.scoped_cooldowns
            .iter()
            .any(|c| c.scope.matches_label(label) && c.until > now)
    }

    /// The live Fable-scoped cooldown's expiry, if any — for the blocking
    /// reason / status readout.
    pub fn fable_cooldown_until(&self, now: SystemTime) -> Option<SystemTime> {
        self.scoped_cooldowns
            .iter()
            .find(|c| c.scope.matches_label(crate::routing::FABLE_SCOPE_LABEL) && c.until > now)
            .map(|c| c.until)
    }

    /// Whether a Fable request must avoid this account right now (fable-usage
    /// W2): a live Fable-scoped cooldown OR the preemptive Fable-critical
    /// exclusion. Non-Fable requests IGNORE this entirely.
    pub fn fable_cooldown_active(&self, now: SystemTime) -> bool {
        self.scoped_cooldown_active(crate::routing::FABLE_SCOPE_LABEL, now)
    }

    /// Preemptive Fable-routing exclusion (W2 point 4): this account's Fable
    /// weekly bucket is currently constraining (`is_active` / critical / over
    /// `threshold`, reset-aware), so Fable requests should avoid it while
    /// non-Fable traffic stays eligible. `threshold` is the account's
    /// EFFECTIVE Fable ceiling (config `scheduler.fable_weekly_max`, default
    /// 0.98, overridable per account via `account_limits`).
    pub fn fable_weekly_exhausted(&self, now: SystemTime, threshold: f64) -> bool {
        self.fable_weekly()
            .is_some_and(|s| s.is_constraining(now, threshold))
    }
}

/// Read-only projection of the whole pool. `select::pick` takes this plus an
/// explicit `now` — it must never read the clock or any shared state itself.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PoolSnapshot {
    pub accounts: Vec<AccountSnapshot>,
    /// Current account per backend group (see [`PoolState::current`]).
    pub current: BTreeMap<BackendGroup, AccountId>,
    /// Fable-scope current per backend group (see [`PoolState::fable_current`]).
    pub fable_current: BTreeMap<BackendGroup, AccountId>,
    /// Manual operator pin per backend group (see [`PoolState::manual_pin`]).
    pub manual_pin: BTreeMap<BackendGroup, ManualPin>,
}

/// Shared handle around `PoolState`. Cheap to clone; every method takes the
/// lock briefly and never does IO under it.
#[derive(Clone)]
pub struct AccountPool {
    inner: Arc<RwLock<PoolState>>,
}

impl std::fmt::Debug for AccountPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountPool").finish_non_exhaustive()
    }
}

/// No account is currently usable. Carries WHY (issue #71): a transient
/// cooldown park answers with its own seconds-scale expiry, a genuine
/// window/health exhaustion answers with the soonest window reset — so the
/// proxy never advertises a window-reset-scale `retry-after` for an 8s park.
#[derive(Debug, thiserror::Error)]
#[error("no account available (retry after {retry_after:?})")]
pub struct NoAccountAvailable {
    pub retry_after: Option<Duration>,
    /// Transient cooldown park vs real quota/health exhaustion.
    pub kind: select::ExhaustionKind,
    /// How many accounts the answer speaks for (in-scope count, not the
    /// whole multi-group pool).
    pub eligible: usize,
}

impl AccountPool {
    pub fn new(accounts: &[AccountConfig]) -> Self {
        Self {
            inner: Arc::new(RwLock::new(PoolState::from_accounts(accounts))),
        }
    }

    pub fn snapshot(&self) -> PoolSnapshot {
        self.read().snapshot()
    }

    /// Credential of one account, cloned (the usage poller needs the live
    /// access token; the snapshot intentionally carries only the kind).
    pub fn credential(&self, account: &AccountId) -> Option<AccountCredential> {
        self.read()
            .accounts
            .iter()
            .find(|a| &a.id == account)
            .map(|a| a.credential.clone())
    }

    /// Lease the CURRENT account for one request within `group` (or the
    /// legacy single slot when `group` is `None`): clones its credential and
    /// increments `in_flight`. The lease pins the account for the request's
    /// lifetime — switching away never affects live leases. Errors when no
    /// account is selected for the group or the current one is hard-down
    /// (auth failure / active cooldown); threshold drift is left to the
    /// evaluation tick, per session stickiness.
    ///
    /// `params` lets the usability check honor heuristic-degraded mode: when
    /// the WHOLE group is parked by retry-after-less (Heuristic) 429s the
    /// selector picks the soonest-freed account, and this lease must accept it
    /// rather than re-reject it on the raw `cooldown_until`. RetryAfter parks
    /// and auth failure still refuse the lease.
    pub fn lease_for(
        &self,
        group: Option<BackendGroup>,
        params: &select::SelectParams,
    ) -> Result<AccountLease, NoAccountAvailable> {
        self.lease_for_scoped(group, params, select::RequestScope::NonFable)
    }

    /// Scope-aware [`Self::lease_for`] (fable-usage W2): a Fable request also
    /// refuses the sticky current when that account holds a live Fable-scoped
    /// cooldown or is preemptively Fable-excluded — so it rotates to a
    /// Fable-eligible account instead of re-hitting the exhausted one.
    /// `RequestScope::NonFable` is exactly the pre-W2 behavior (Fable state
    /// ignored).
    pub fn lease_for_scoped(
        &self,
        group: Option<BackendGroup>,
        params: &select::SelectParams,
        scope: select::RequestScope,
    ) -> Result<AccountLease, NoAccountAvailable> {
        let slot = group.unwrap_or(LEGACY_GROUP);
        let now = SystemTime::now();
        let mut state = self.write();
        let no_account = |state: &PoolState| {
            let snapshot = state.snapshot();
            let (kind, eligible) =
                select::classify_exhaustion(&snapshot, params, group, scope, now);
            let retry_after = match kind {
                select::ExhaustionKind::CooldownBlocked { min_expiry, .. } => Some(min_expiry),
                select::ExhaustionKind::WindowBlocked => select::soonest_reset(&snapshot, now),
            };
            NoAccountAvailable {
                retry_after,
                kind,
                eligible,
            }
        };
        // Lease the sticky current of the request's OWN scope: a Fable request
        // reads `fable_current`, a non-Fable request `current`.
        let scope_current = match scope {
            select::RequestScope::Fable => state.fable_current.get(&slot),
            _ => state.current.get(&slot),
        };
        let Some(current) = scope_current.cloned() else {
            return Err(no_account(&state));
        };
        // An active manual pin on the leased current (issue #122) relaxes
        // the preemptive Fable-weekly exclusion below: the operator's
        // explicit choice serves until a REAL failure.
        let pinned_current = state
            .manual_pin
            .get(&slot)
            .is_some_and(|pin| pin.account == current && pin.until > now);
        let snapshot = state.snapshot();
        let headers_only = select::headers_only_mode(&snapshot, params, group, now);
        let heuristic_degraded = select::heuristic_degraded_mode(&snapshot, params, group, now);
        let unusable = match snapshot.accounts.iter().find(|a| a.id == current) {
            // Reuse the pure gate so the lease agrees with what the selector
            // chose. Auth failure, operator pause and a RetryAfter cooldown
            // always refuse; a Heuristic cooldown refuses UNLESS the group is
            // in degraded mode. 5h/7d/staleness gates are evaluation-tick
            // concerns, not a reason to refuse a request already routed to
            // the sticky current — so ignore those reasons here. A Fable
            // request ALSO refuses on its scope-specific gates (Fable
            // cooldown / Fable preemptive exclusion), so it never leases a
            // Fable-dead current. Paused is in the refuse set because pause
            // is the operator's ABSOLUTE bench (issue #121): the fable slot
            // used to keep leasing a paused sticky current indefinitely —
            // the NonFable slot healed on the next evaluation tick, but
            // nothing re-evaluated `fable_current`.
            Some(acct) => match select::gate_scoped(
                acct,
                params,
                now,
                headers_only,
                heuristic_degraded,
                scope,
            ) {
                Some(select::IneligibleReason::FableWeeklyExhausted) => !pinned_current,
                Some(reason) => reason.hard_failure(),
                None => false,
            },
            None => true,
        };
        if unusable {
            return Err(no_account(&state));
        }
        // Re-borrow mutably now that the immutable checks are done.
        let Some(acct) = state.account_mut(&current) else {
            return Err(NoAccountAvailable {
                retry_after: None,
                kind: select::ExhaustionKind::WindowBlocked,
                eligible: 0,
            });
        };
        acct.in_flight = acct.in_flight.saturating_add(1);
        let lease = AccountLease {
            pool: Arc::clone(&self.inner),
            id: acct.id.clone(),
            credential: acct.credential.clone(),
        };
        Ok(lease)
    }

    /// Run the pure selector over a fresh snapshot and commit the resulting
    /// decision (CAS). Returns the decision actually applied. This is the
    /// ONLY entry point that changes `current` — called from the periodic
    /// re-evaluation tick and from the 429/ineligibility paths, never
    /// per-request. Snapshot, decision, and commit happen under ONE write
    /// lock, so the CAS cannot race (the CAS in `commit_switch` still guards
    /// direct external callers).
    pub fn evaluate(
        &self,
        group: Option<BackendGroup>,
        params: &select::SelectParams,
        now: SystemTime,
    ) -> select::Decision {
        self.evaluate_scoped(group, params, now, select::RequestScope::NonFable)
    }

    /// Scope-aware [`Self::evaluate`] (fable-usage W2): runs the scope-aware
    /// selector and commits with the SAME scope, so a Fable request selects and
    /// pins a Fable-eligible account (avoiding Fable-cooling / Fable-critical
    /// accounts) while non-Fable selection is unchanged.
    pub fn evaluate_scoped(
        &self,
        group: Option<BackendGroup>,
        params: &select::SelectParams,
        now: SystemTime,
        scope: select::RequestScope,
    ) -> select::Decision {
        let slot = group.unwrap_or(LEGACY_GROUP);
        let mut state = self.write();
        let snapshot = state.snapshot();
        let decision = select::pick_scoped(&snapshot, params, group, now, scope);
        match &decision {
            select::Decision::Stay => decision,
            select::Decision::Switch { to } => {
                let expected = snapshot.current_for_scope(slot, scope);
                match state.commit_switch_scoped(group, expected, to, params, now, scope) {
                    Ok(()) => decision,
                    // Unreachable while the write lock is held (pick and
                    // commit see the same state) — degrade honestly anyway.
                    Err(err) => {
                        tracing::error!(?err, "commit_switch failed under evaluate lock");
                        select::Decision::Exhausted {
                            retry_after: select::soonest_reset(&snapshot, now),
                        }
                    }
                }
            }
            select::Decision::Exhausted { .. } => {
                // Nothing usable in this group FOR THIS SCOPE: clear the scope's
                // own slot so lease_for refuses until a later evaluation finds
                // capacity again. A Fable exhaustion never clears the non-Fable
                // current (and vice versa).
                match scope {
                    select::RequestScope::Fable => state.fable_current.remove(&slot),
                    _ => state.current.remove(&slot),
                };
                decision
            }
        }
    }

    /// Manual switch to an explicit target (TUI `s`): validates eligibility
    /// via the same pure gate the selector uses and commits like
    /// [`PoolState::commit_switch`] — snapshot, gate, and commit happen under
    /// ONE write lock, so the CAS against `current` cannot race. Lease-guard
    /// semantics are preserved: in-flight requests keep their pinned
    /// account/credential until Drop; only NEW leases land on the target.
    pub fn switch_to(
        &self,
        target: &AccountId,
        params: &select::SelectParams,
        now: SystemTime,
    ) -> Result<(), SwitchError> {
        let mut state = self.write();
        // A manual switch lands the target into ITS OWN group's slot (derived
        // from the target's credential kind) — so switching a codex account
        // never displaces the claude slot and vice versa. An unknown target
        // is reported by commit_switch.
        let group = state
            .accounts
            .iter()
            .find(|a| &a.id == target)
            .map(|a| BackendGroup::from_kind(a.credential.kind()));
        let Some(group) = group else {
            return Err(SwitchError::UnknownAccount(target.clone()));
        };
        let expected = state.current.get(&group).cloned();
        // A manual switch is an informed operator decision (issue #122):
        // pin the group — Fable lane included — for MANUAL_PIN_DURATION, so
        // the evaluation ticks cannot immediately override it. Only a real
        // failure (429 park / auth failure / pause) or expiry releases it.
        // The pin is inserted BEFORE the commit on purpose: the commit's
        // re-validation relaxes to hard failures for a pinned target, which
        // is exactly the operator-override contract — a manual switch to an
        // over-threshold account must succeed ("send it until it actually
        // errors"), while auth-dead / paused / parked targets still refuse.
        // A refused commit rolls the pin back.
        state.manual_pin.insert(
            group,
            ManualPin {
                account: target.clone(),
                until: now
                    .checked_add(MANUAL_PIN_DURATION)
                    .unwrap_or(SystemTime::UNIX_EPOCH),
            },
        );
        if let Err(err) = state.commit_switch(Some(group), expected.as_ref(), target, params, now) {
            state.manual_pin.remove(&group);
            return Err(err);
        }
        // The fable slot follows the operator's choice at once, so the very
        // next fable request lands on the chosen account rather than
        // waiting a tick.
        state.fable_current.insert(group, target.clone());
        Ok(())
    }

    pub fn record_headers(
        &self,
        account: &AccountId,
        parsed: &ParsedRateLimitHeaders,
        now: SystemTime,
    ) {
        self.write().record_headers(account, parsed, now);
    }

    pub fn record_usage(&self, account: &AccountId, usage: &UsageSnapshot, now: SystemTime) {
        self.write().record_usage(account, usage, now);
    }

    /// See [`PoolState::reset_usage`] — the `POST /llmux/reset-usage`
    /// operator command (issue #115).
    pub fn reset_usage(&self) -> usize {
        self.write().reset_usage()
    }

    pub fn record_429(&self, account: &AccountId, retry_after: Option<Duration>, now: SystemTime) {
        self.write().record_429(account, retry_after, now);
    }

    /// Scope-aware 429 recording (fable-usage W2). See
    /// [`PoolState::record_429_classified`]; returns the classification reason
    /// for logging / status.
    pub fn record_429_classified(
        &self,
        account: &AccountId,
        retry_after: Option<Duration>,
        model: Option<&str>,
        now: SystemTime,
    ) -> Cooldown429Reason {
        self.write()
            .record_429_classified(account, retry_after, model, now)
    }

    pub fn record_auth_failure(&self, account: &AccountId) {
        self.write().record_auth_failure(account);
    }

    pub fn update_credential(&self, account: &AccountId, credential: AccountCredential) {
        self.write().update_credential(account, credential);
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, PoolState> {
        self.inner.read().expect("pool lock poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, PoolState> {
        self.inner.write().expect("pool lock poisoned")
    }

    /// Replace the account roster after a config reload (TUI `R`, `import`
    /// while running). Existing window/cooldown state is kept for accounts
    /// that survive (credentials refresh from config); leases on removed
    /// accounts drain naturally. A removed `current` clears the selection.
    pub fn reload_accounts(&self, accounts: &[AccountConfig]) {
        let mut state = self.write();
        let next: Vec<AccountState> = accounts
            .iter()
            .map(|config| {
                let id = AccountId(config.name.clone());
                match state.accounts.iter().find(|a| a.id == id) {
                    Some(existing) => {
                        let mut kept = existing.clone();
                        kept.credential = config.credential.clone();
                        kept
                    }
                    None => AccountState::fresh(config),
                }
            })
            .collect();
        // Drop any group slot whose current account no longer exists; other
        // slots keep their sticky selection. Same for manual pins (#122) and
        // the fable slot.
        state
            .current
            .retain(|_, current| next.iter().any(|a| &a.id == current));
        state
            .fable_current
            .retain(|_, current| next.iter().any(|a| &a.id == current));
        state
            .manual_pin
            .retain(|_, pin| next.iter().any(|a| a.id == pin.account));
        state.accounts = next;
    }

    /// Apply the operator pause set (config `paused_accounts`) to the live
    /// pool — called next to [`Self::reload_accounts`] whenever the config is
    /// (re)applied, so the config file stays the single source of truth. A
    /// paused CURRENT account is not force-switched here; the next
    /// `evaluate` tick sees it ineligible and moves off it cooperatively.
    pub fn apply_paused(&self, paused: &std::collections::BTreeSet<String>) {
        let mut state = self.write();
        for account in &mut state.accounts {
            account.paused = paused.contains(&account.id.0);
        }
        // Pausing a manually pinned account ENDS the pin (issue #122 review
        // M1): pause is a real operator failure signal, and a merely-blocked
        // pin would re-grab the account the moment it is resumed inside the
        // pin window — the resume must hand control back to automatic
        // scheduling, not to a stale override.
        state
            .manual_pin
            .retain(|_, pin| !paused.contains(&pin.account.0));
    }

    /// Apply the per-account ceiling overrides (config `account_limits`) to
    /// the live pool — same config-is-SSOT convention as [`Self::apply_paused`].
    pub fn apply_limits(
        &self,
        limits: &std::collections::BTreeMap<String, crate::config::AccountLimits>,
    ) {
        let mut state = self.write();
        for account in &mut state.accounts {
            account.limits = limits.get(&account.id.0).copied().unwrap_or_default();
        }
    }
}

/// Drop-based guard pinning one account for one in-flight request.
/// Holds a CLONE of the credential taken at lease time — a concurrent
/// credential refresh or switch does not change what this request sends.
pub struct AccountLease {
    pool: Arc<RwLock<PoolState>>,
    id: AccountId,
    credential: AccountCredential,
}

/// Manual impl: never print the pinned credential (it holds live secrets).
impl std::fmt::Debug for AccountLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountLease")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl AccountLease {
    pub fn account_id(&self) -> &AccountId {
        &self.id
    }

    pub fn credential(&self) -> &AccountCredential {
        &self.credential
    }
}

impl Drop for AccountLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.pool.write() {
            if let Some(acct) = state.accounts.iter_mut().find(|a| a.id == self.id) {
                acct.in_flight = acct.in_flight.saturating_sub(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use select::{Decision, IneligibleReason, SelectParams};

    const NOW_SECS: u64 = 1_000_000;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn now() -> SystemTime {
        at(NOW_SECS)
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

    fn id(s: &str) -> AccountId {
        AccountId(s.to_string())
    }

    fn reading(utilization: f64, resets_at_secs: u64) -> WindowReading {
        WindowReading {
            utilization,
            resets_at: at(resets_at_secs),
        }
    }

    fn usage(five: Option<WindowReading>, seven: Option<WindowReading>) -> UsageSnapshot {
        UsageSnapshot {
            five_hour: five,
            seven_day: seven,
            scoped: Vec::new(),
        }
    }

    #[test]
    fn from_accounts_starts_cold_and_healthy() {
        let state = PoolState::from_accounts(&[oauth_account("a"), apikey_account("b")]);
        assert_eq!(state.accounts.len(), 2);
        assert!(state.current.is_empty());
        for acct in &state.accounts {
            assert_eq!(acct.health, AccountHealth::Healthy);
            assert!(acct.five_hour.is_none());
            assert!(acct.seven_day.is_none());
            assert!(acct.cooldown_until.is_none());
            assert_eq!(acct.in_flight, 0);
        }
    }

    /// Issue #121 (live defect 2026-07-16): a PAUSED fable sticky current
    /// kept serving new fable leases indefinitely — `Paused` was missing
    /// from the lease refuse set, and nothing re-evaluated `fable_current`.
    /// The full heal cycle: pause refuses the lease, the fable evaluation
    /// moves the slot, the next lease lands on the healthy account.
    #[test]
    fn paused_fable_current_is_refused_and_heals_on_the_fable_tick() {
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        pool.evaluate(None, &params(), now());
        assert_eq!(
            pool.evaluate_scoped(None, &params(), now(), select::RequestScope::Fable),
            Decision::Switch { to: id("a") },
            "fable slot seeds from the account current"
        );
        assert_eq!(
            pool.lease_for_scoped(None, &params(), select::RequestScope::Fable)
                .expect("healthy current leases")
                .id,
            id("a")
        );

        pool.apply_paused(&std::collections::BTreeSet::from(["a".to_string()]));
        assert!(
            pool.lease_for_scoped(None, &params(), select::RequestScope::Fable)
                .is_err(),
            "a paused fable current must refuse new leases"
        );
        // The fable-scope evaluation (now run by the periodic tick, issue
        // #121) moves the slot off the paused account…
        assert_eq!(
            pool.evaluate_scoped(None, &params(), now(), select::RequestScope::Fable),
            Decision::Switch { to: id("b") }
        );
        // …and leases flow again.
        assert_eq!(
            pool.lease_for_scoped(None, &params(), select::RequestScope::Fable)
                .expect("healed")
                .id,
            id("b")
        );
    }

    /// Issue #122: a manual switch pins BOTH scopes on the chosen account
    /// for MANUAL_PIN_DURATION — the evaluation ticks stay, an
    /// over-threshold target is accepted (operator override), and only a
    /// real failure (recorded 429) releases the pin.
    #[test]
    fn manual_switch_pins_both_scopes_until_a_real_error() {
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        pool.evaluate(None, &params(), now());
        // Make "b" clearly over the 5h ceiling — automatic selection would
        // never pick it, and the OLD manual switch would refuse it.
        pool.record_usage(
            &id("b"),
            &usage(Some(reading(0.95, NOW_SECS + 3600)), None),
            now(),
        );
        pool.switch_to(&id("b"), &params(), now())
            .expect("operator override lands on an over-threshold account");
        // Pinned: the evaluation tick must NOT move off b, either scope.
        assert_eq!(pool.evaluate(None, &params(), now()), Decision::Stay);
        assert_eq!(
            pool.evaluate_scoped(None, &params(), now(), select::RequestScope::Fable),
            Decision::Stay,
            "the fable slot followed the manual switch"
        );
        assert_eq!(
            pool.lease_for_scoped(None, &params(), select::RequestScope::Fable)
                .expect("pinned account serves fable")
                .id,
            id("b")
        );
        // A real error releases the pin: automatic selection resumes.
        pool.record_429(&id("b"), Some(Duration::from_secs(120)), now());
        assert_eq!(
            pool.evaluate(None, &params(), now()),
            Decision::Switch { to: id("a") },
            "the 429 broke the pin and selection moved on"
        );
    }

    /// Review M1 (PR #124): PAUSING a pinned account ENDS the pin — a
    /// resume inside the pin window must hand control back to automatic
    /// scheduling, not to the stale override.
    #[test]
    fn pausing_a_pinned_account_clears_the_pin() {
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        pool.evaluate(None, &params(), now());
        pool.switch_to(&id("b"), &params(), now()).expect("switch");
        // Pause b: the pin is hard-blocked AND cleared; the next evaluation
        // moves off b.
        pool.apply_paused(&std::collections::BTreeSet::from(["b".to_string()]));
        assert_eq!(
            pool.evaluate(None, &params(), now()),
            Decision::Switch { to: id("a") },
            "pause benches the pinned account"
        );
        // Resume b INSIDE the pin window: with the pin gone, stickiness
        // keeps the traffic where automatic scheduling put it.
        pool.apply_paused(&std::collections::BTreeSet::new());
        assert_eq!(
            pool.evaluate(None, &params(), now()),
            Decision::Stay,
            "resume must NOT re-grab the account through a stale pin"
        );
        assert_eq!(snap_legacy(&pool), Some(id("a")));
    }

    /// Issue #122: the pin lapses after MANUAL_PIN_DURATION — automatic
    /// perishability scheduling resumes on its own.
    #[test]
    fn manual_pin_expires_after_its_duration() {
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        pool.evaluate(None, &params(), now());
        pool.record_usage(
            &id("b"),
            &usage(Some(reading(0.95, NOW_SECS + 3600)), None),
            now(),
        );
        pool.switch_to(&id("b"), &params(), now()).expect("switch");
        let later = now() + MANUAL_PIN_DURATION + Duration::from_secs(1);
        assert_eq!(
            pool.evaluate(None, &params(), later),
            Decision::Switch { to: id("a") },
            "after expiry the over-threshold account is abandoned again"
        );
    }

    /// Issue #115: the operator reset returns every account to the cold shape
    /// above — windows/scoped limits/cooldowns gone — while operator state
    /// (pause, per-account ceilings) survives.
    #[test]
    fn reset_usage_clears_windows_and_cooldowns_but_keeps_operator_state() {
        let mut state = PoolState::from_accounts(&[oauth_account("a"), oauth_account("b")]);
        state.record_usage(
            &id("a"),
            &usage(
                Some(reading(0.87, NOW_SECS + 3600)),
                Some(reading(0.42, NOW_SECS + 86_400)),
            ),
            now(),
        );
        state.record_429(&id("b"), None, now());
        state.accounts[1].paused = true;
        assert!(state.accounts[0].five_hour.is_some(), "seed took");
        assert!(state.accounts[1].cooldown_until.is_some(), "seed took");

        assert_eq!(state.reset_usage(), 2, "reports the accounts reset");
        for acct in &state.accounts {
            assert!(acct.five_hour.is_none(), "{}: 5h window cold", acct.id.0);
            assert!(acct.seven_day.is_none(), "{}: 7d window cold", acct.id.0);
            assert!(
                acct.scoped_limits.is_empty(),
                "{}: scoped limits",
                acct.id.0
            );
            assert!(
                acct.scoped_cooldowns.is_empty(),
                "{}: scoped cooldowns",
                acct.id.0
            );
            assert!(acct.cooldown_until.is_none(), "{}: cooldown", acct.id.0);
            assert!(acct.cooldown_source.is_none(), "{}: source", acct.id.0);
            assert!(acct.cooldown_set_at.is_none(), "{}: set_at", acct.id.0);
        }
        assert!(
            state.accounts[1].paused,
            "pause is operator state, not usage — must survive the reset"
        );
    }

    #[test]
    fn record_headers_merges_unified_windows() {
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        let parsed = ParsedRateLimitHeaders {
            five_hour: Some(reading(0.42, NOW_SECS + 3600)),
            seven_day: Some(reading(0.87, NOW_SECS + 86_400)),
            ..Default::default()
        };
        state.record_headers(&id("a"), &parsed, now());
        let acct = &state.accounts[0];
        assert_eq!(acct.five_hour.unwrap().utilization, 0.42);
        assert_eq!(acct.five_hour.unwrap().source, WindowSource::Headers);
        assert_eq!(acct.seven_day.unwrap().utilization, 0.87);
        assert_eq!(acct.five_hour.unwrap().fetched_at, now());
    }

    /// Issue #123: the `7d_oi` header reading lands in the Fable scoped slot
    /// — a COLD gauge heals on the first response through the account, and
    /// the scheduler's Fable exclusion sees the same number.
    #[test]
    fn record_headers_populates_the_fable_scoped_gauge() {
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        assert!(state.accounts[0].scoped_limits.is_empty(), "starts cold");
        let parsed = ParsedRateLimitHeaders {
            fable_weekly: Some(reading(0.97, NOW_SECS + 86_400)),
            ..Default::default()
        };
        state.record_headers(&id("a"), &parsed, now());
        let snap = state.snapshot();
        let fable = snap.accounts[0].fable_weekly().expect("gauge healed");
        assert_eq!(fable.window.utilization, 0.97);
        assert_eq!(fable.window.source, WindowSource::Headers);
        assert_eq!(fable.severity, window::LimitSeverity::Critical);
        assert!(
            snap.accounts[0].fable_weekly_exhausted(now(), 0.95),
            "the header-fed gauge drives the preemptive Fable exclusion"
        );
    }

    #[test]
    fn stale_observation_does_not_overwrite_fresher_one() {
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        let fresh = ParsedRateLimitHeaders {
            five_hour: Some(reading(0.50, NOW_SECS + 3600)),
            ..Default::default()
        };
        state.record_headers(&id("a"), &fresh, at(NOW_SECS + 100));
        // An older (out-of-order) observation must not win.
        let older = ParsedRateLimitHeaders {
            five_hour: Some(reading(0.10, NOW_SECS + 3600)),
            ..Default::default()
        };
        state.record_headers(&id("a"), &older, now());
        assert_eq!(state.accounts[0].five_hour.unwrap().utilization, 0.50);
    }

    #[test]
    fn standard_headers_feed_five_hour_slot_when_unified_absent() {
        let mut state = PoolState::from_accounts(&[apikey_account("k")]);
        let parsed = ParsedRateLimitHeaders {
            standard: Some(headers::StandardRateLimit {
                requests_limit: Some(100),
                requests_remaining: Some(20),
                requests_reset: Some(at(NOW_SECS + 60)),
                tokens_limit: None,
                tokens_remaining: None,
                tokens_reset: None,
            }),
            ..Default::default()
        };
        state.record_headers(&id("k"), &parsed, now());
        let window = state.accounts[0].five_hour.unwrap();
        assert!((window.utilization - 0.80).abs() < 1e-9);
        assert_eq!(window.resets_at, at(NOW_SECS + 60));
    }

    fn reset_less_grok_standard() -> ParsedRateLimitHeaders {
        // grok shape: limit/remaining with no reset (900 limit / 720 remaining
        // → utilization 0.20).
        ParsedRateLimitHeaders {
            standard: Some(headers::StandardRateLimit {
                requests_limit: Some(900),
                requests_remaining: Some(720),
                requests_reset: None,
                tokens_limit: None,
                tokens_remaining: None,
                tokens_reset: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn reset_less_standard_headers_use_fallback_horizon_for_grok_only() {
        // grok account: reset-less reading gets the estimated
        // STANDARD_RESET_FALLBACK horizon from now.
        let parsed = reset_less_grok_standard();
        let mut state = PoolState::from_accounts(&[grok_account("g")]);
        state.record_headers(&id("g"), &parsed, now());
        let window = state.accounts[0].five_hour.unwrap();
        assert!((window.utilization - 0.20).abs() < 1e-9);
        assert_eq!(window.resets_at, now() + headers::STANDARD_RESET_FALLBACK);

        // codex / oauth accounts: the provider-generic names must NOT inject a
        // synthetic-horizon window — a reset-less reading is dropped.
        for account in [codex_account("cx"), oauth_account("o")] {
            let name = account.name.clone();
            let mut state = PoolState::from_accounts(&[account]);
            state.record_headers(&id(&name), &parsed, now());
            assert!(
                state.accounts[0].five_hour.is_none(),
                "{name}: reset-less standard reading must be dropped for non-grok"
            );
        }
    }

    #[test]
    fn grok_x_ratelimit_capture_parses_and_records_via_record_headers() {
        // End-to-end: exact 2026-07-14 cli-chat-proxy.grok.com capture (HTTP
        // 200) through headers::parse then record_headers on a grok account.
        let mut headers_map = http::HeaderMap::new();
        for (name, value) in [
            ("x-ratelimit-limit-tokens", "15000000"),
            ("x-ratelimit-remaining-tokens", "15000000"),
            ("x-ratelimit-limit-requests", "900"),
            ("x-ratelimit-remaining-requests", "900"),
        ] {
            headers_map.insert(
                name.parse::<http::HeaderName>().unwrap(),
                http::HeaderValue::from_str(value).unwrap(),
            );
        }
        let parsed = headers::parse(&headers_map);
        let mut state = PoolState::from_accounts(&[grok_account("g")]);
        state.record_headers(&id("g"), &parsed, now());
        let window = state.accounts[0].five_hour.unwrap();
        assert_eq!(window.utilization, 0.0);
        assert_eq!(window.resets_at, now() + headers::STANDARD_RESET_FALLBACK);
    }

    #[test]
    fn record_429_with_retry_after_parks_exactly() {
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        state.record_429(&id("a"), Some(Duration::from_secs(2)), now());
        let acct = &state.accounts[0];
        assert_eq!(acct.cooldown_until, Some(at(NOW_SECS + 2)));
        assert_eq!(acct.cooldown_source, Some(CooldownSource::RetryAfter));
    }

    #[test]
    fn record_429_without_retry_after_uses_heuristic_default() {
        // The transient (retry-after-less) park is SHORT: 8s, not the old 30s —
        // a retry-after-less 429 is a per-minute-window blip, and degraded-mode
        // selection serves the soonest-freed account meanwhile.
        assert_eq!(DEFAULT_HEURISTIC_COOLDOWN, Duration::from_secs(8));
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        state.record_429(&id("a"), None, now());
        let acct = &state.accounts[0];
        assert_eq!(
            acct.cooldown_until,
            Some(at(NOW_SECS + DEFAULT_HEURISTIC_COOLDOWN.as_secs()))
        );
        assert_eq!(acct.cooldown_source, Some(CooldownSource::Heuristic));
    }

    #[test]
    fn fresh_usage_with_capacity_clears_heuristic_cooldown() {
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        state.record_429(&id("a"), None, now());
        // Later poll shows both windows under 100%.
        state.record_usage(
            &id("a"),
            &usage(
                Some(reading(0.30, NOW_SECS + 3600)),
                Some(reading(0.50, NOW_SECS + 86_400)),
            ),
            at(NOW_SECS + 300),
        );
        let acct = &state.accounts[0];
        assert!(
            acct.cooldown_until.is_none(),
            "heuristic cooldown self-heals"
        );
        assert!(acct.cooldown_source.is_none());
    }

    #[test]
    fn same_instant_data_cannot_heal_its_own_cooldown() {
        // The 429 response itself carries headers; those must not clear the
        // cooldown the same response just set.
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        state.record_429(&id("a"), None, now());
        state.record_usage(
            &id("a"),
            &usage(Some(reading(0.30, NOW_SECS + 3600)), None),
            now(),
        );
        assert!(state.accounts[0].cooldown_until.is_some());
    }

    #[test]
    fn usage_at_full_capacity_does_not_heal() {
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        state.record_429(&id("a"), None, now());
        state.record_usage(
            &id("a"),
            &usage(Some(reading(1.0, NOW_SECS + 3600)), None),
            at(NOW_SECS + 300),
        );
        assert!(state.accounts[0].cooldown_until.is_some());
    }

    #[test]
    fn retry_after_park_is_never_healed_by_data() {
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        state.record_429(&id("a"), Some(Duration::from_secs(600)), now());
        state.record_usage(
            &id("a"),
            &usage(Some(reading(0.0, NOW_SECS + 3600)), None),
            at(NOW_SECS + 300),
        );
        assert!(
            state.accounts[0].cooldown_until.is_some(),
            "explicit retry-after park must run its full course"
        );
    }

    // ---- scope-aware 429 classification + Fable cooldown separation (W2) ----

    /// A Fable weekly `limits[]` reading for the account's scoped list.
    fn fable_reading(util: f64, active: bool, critical: bool) -> ScopedLimitReading {
        ScopedLimitReading {
            scope_label: "Fable".into(),
            reading: reading(util, NOW_SECS + 86_400),
            severity: if critical {
                window::LimitSeverity::Critical
            } else {
                window::LimitSeverity::Normal
            },
            is_active: active,
        }
    }

    fn usage_with_fable(
        five: Option<WindowReading>,
        seven: Option<WindowReading>,
        fable: ScopedLimitReading,
    ) -> UsageSnapshot {
        UsageSnapshot {
            five_hour: five,
            seven_day: seven,
            scoped: vec![fable],
        }
    }

    /// THE mandatory W0 regression test: a Fable 429 (no Retry-After, no unified
    /// headers) while all-models weekly is NOT critical must bench ONLY the
    /// Fable scope — the account stays eligible for a non-Fable (haiku) request
    /// and is excluded ONLY for a Fable request, by the Fable-scoped cooldown.
    #[test]
    fn fable_429_benches_only_fable_scope_leaving_the_account_eligible_for_non_fable() {
        use select::{gate_scoped, IneligibleReason, RequestScope};
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        // All-models weekly well under threshold; no Fable snapshot at all.
        state.record_usage(
            &id("a"),
            &usage(
                Some(reading(0.0, NOW_SECS + 3600)),
                Some(reading(0.40, NOW_SECS + 86_400)),
            ),
            now(),
        );

        // Fable-family 429, no retry-after, no headers.
        let recorded = state.record_429_classified(&id("a"), None, Some("fable-5"), now());
        // Snapshot said Fable OK → suspect mismatch, but the Fable-scoped
        // cooldown is set REGARDLESS (never defaulted to account-wide).
        assert_eq!(recorded, Cooldown429Reason::FableSuspectSnapshotMismatch);
        assert!(
            state.accounts[0].cooldown_until.is_none(),
            "a Fable 429 must NOT park the whole account"
        );
        assert_eq!(state.accounts[0].scoped_cooldowns.len(), 1);

        let snapshot = state.snapshot();
        let a = &snapshot.accounts[0];
        let p = params();
        // Non-Fable (haiku) request: A stays eligible.
        assert_eq!(
            gate_scoped(a, &p, now(), false, false, RequestScope::NonFable),
            None,
            "non-Fable request stays eligible despite the Fable-only 429"
        );
        // Fable request: A is excluded by the Fable-scoped cooldown.
        assert_eq!(
            gate_scoped(a, &p, now(), false, false, RequestScope::Fable),
            Some(IneligibleReason::FableCoolingDown),
            "Fable request is benched by the Fable-scoped cooldown"
        );
        // The scoped park is transient (self-limits by DEFAULT_HEURISTIC_COOLDOWN).
        let after = at(NOW_SECS + DEFAULT_HEURISTIC_COOLDOWN.as_secs());
        assert_eq!(
            gate_scoped(a, &p, after, false, false, RequestScope::Fable),
            None,
            "Fable-scoped cooldown lifts once its park expires"
        );
    }

    #[test]
    fn non_fable_429_without_retry_after_still_parks_the_whole_account() {
        // Regression preservation: the pre-W2 whole-account heuristic park is
        // unchanged for non-Fable (scope-blind corroboration) 429s.
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        let recorded = state.record_429_classified(&id("a"), None, Some("claude-haiku-4-5"), now());
        assert_eq!(recorded, Cooldown429Reason::AccountAllObserved);
        assert_eq!(
            state.accounts[0].cooldown_until,
            Some(at(NOW_SECS + DEFAULT_HEURISTIC_COOLDOWN.as_secs()))
        );
        assert_eq!(
            state.accounts[0].cooldown_source,
            Some(CooldownSource::Heuristic)
        );
        assert!(state.accounts[0].scoped_cooldowns.is_empty());
    }

    #[test]
    fn retry_after_fable_429_parks_account_wide_not_scoped() {
        // A retry-after 429 is an explicit upstream instruction → account-wide
        // RetryAfter park as today, even for a Fable request.
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        let recorded = state.record_429_classified(
            &id("a"),
            Some(Duration::from_secs(60)),
            Some("fable-5"),
            now(),
        );
        assert_eq!(recorded, Cooldown429Reason::HeaderRetryAfter);
        assert_eq!(
            state.accounts[0].cooldown_source,
            Some(CooldownSource::RetryAfter)
        );
        assert!(
            state.accounts[0].scoped_cooldowns.is_empty(),
            "retry-after Fable 429 parks account-wide, not scoped"
        );
    }

    #[test]
    fn fable_429_with_corroborating_snapshot_reasons_observed_critical() {
        // The dev1 evidence shape: Fable weekly 100% active/critical, all-models
        // 7d only 58% → Fable-scoped cooldown + reason ObservedCritical, and NO
        // account-wide escalation (7d not critical) — non-Fable stays eligible.
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        state.record_usage(
            &id("a"),
            &usage_with_fable(
                Some(reading(0.0, NOW_SECS + 3600)),
                Some(reading(0.58, NOW_SECS + 86_400)),
                fable_reading(1.0, true, true),
            ),
            now(),
        );
        let recorded = state.record_429_classified(&id("a"), None, Some("fable-5"), now());
        assert_eq!(recorded, Cooldown429Reason::FableObservedCritical);
        assert!(
            state.accounts[0].cooldown_until.is_none(),
            "7d not critical → no account-wide escalation"
        );
        let snapshot = state.snapshot();
        let a = &snapshot.accounts[0];
        assert!(a.fable_cooldown_active(now()), "Fable-scoped cooldown set");
        assert!(
            a.fable_weekly_exhausted(now(), 0.98),
            "preemptive Fable exclusion also engaged"
        );
    }

    #[test]
    fn fable_429_escalates_account_wide_when_account_windows_corroborate() {
        // 7d itself reads critical (≥95%): genuine whole-account exhaustion, so
        // the Fable 429 ALSO parks account-wide (belt-and-suspenders over the
        // ceiling gate) — the corroboration escalation path.
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        state.record_usage(
            &id("a"),
            &usage(
                Some(reading(0.0, NOW_SECS + 3600)),
                Some(reading(0.97, NOW_SECS + 86_400)),
            ),
            now(),
        );
        let recorded = state.record_429_classified(&id("a"), None, Some("fable-5"), now());
        // No Fable snapshot → mismatch reason, but escalation is orthogonal.
        assert_eq!(recorded, Cooldown429Reason::FableSuspectSnapshotMismatch);
        assert!(!state.accounts[0].scoped_cooldowns.is_empty());
        assert!(
            state.accounts[0].cooldown_until.is_some(),
            "critical 5h/7d corroborates → account-wide escalation"
        );
        assert_eq!(
            state.accounts[0].cooldown_source,
            Some(CooldownSource::Heuristic)
        );
    }

    #[test]
    fn record_429_classified_on_missing_account_is_unknown_noop() {
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        let recorded = state.record_429_classified(&id("ghost"), None, Some("fable-5"), now());
        assert_eq!(recorded, Cooldown429Reason::Unknown);
    }

    #[test]
    fn cooldown_scope_matches_label_is_case_insensitive() {
        let fable = CooldownScope::ModelScoped("Fable".into());
        assert!(fable.matches_label("fable"));
        assert!(fable.matches_label("FABLE"));
        assert!(!fable.matches_label("Opus"));
        assert_eq!(fable.label(), Some("Fable"));
        assert_eq!(CooldownScope::AccountWide.label(), None);
        assert!(!CooldownScope::AccountWide.matches_label("Fable"));
    }

    #[test]
    fn auth_failure_marks_and_credential_update_heals() {
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        state.record_auth_failure(&id("a"));
        assert_eq!(state.accounts[0].health, AccountHealth::AuthFailed);
        state.update_credential(
            &id("a"),
            AccountCredential::Oauth {
                account_uuid: "uuid-a".into(),
                access_token: "new-at".into(),
                refresh_token: "new-rt".into(),
                expires_at_ms: 9_999,
                tier: None,
                last_refresh_ms: None,
            },
        );
        assert_eq!(state.accounts[0].health, AccountHealth::Healthy);
        match &state.accounts[0].credential {
            AccountCredential::Oauth { access_token, .. } => assert_eq!(access_token, "new-at"),
            other => panic!("unexpected credential {other:?}"),
        }
    }

    /// Legacy (routing-disabled) current — these tests drive the `None`
    /// group, so the single legacy slot holds the selection.
    fn legacy(state: &PoolState) -> Option<AccountId> {
        state.current.get(&LEGACY_GROUP).cloned()
    }

    fn snap_legacy(pool: &AccountPool) -> Option<AccountId> {
        pool.snapshot().legacy_current().cloned()
    }

    #[test]
    fn commit_switch_cas_aborts_on_changed_current() {
        let mut state = PoolState::from_accounts(&[oauth_account("a"), oauth_account("b")]);
        state.current.insert(LEGACY_GROUP, id("a"));
        let err = state
            .commit_switch(None, None, &id("b"), &params(), now())
            .unwrap_err();
        assert_eq!(
            err,
            SwitchError::CurrentChanged {
                actual: Some(id("a")),
            }
        );
        assert_eq!(legacy(&state), Some(id("a")), "nothing mutated on abort");
    }

    #[test]
    fn commit_switch_rejects_unknown_target() {
        let mut state = PoolState::from_accounts(&[oauth_account("a")]);
        let err = state
            .commit_switch(None, None, &id("ghost"), &params(), now())
            .unwrap_err();
        assert_eq!(err, SwitchError::UnknownAccount(id("ghost")));
    }

    #[test]
    fn commit_switch_rejects_target_that_became_ineligible() {
        let mut state = PoolState::from_accounts(&[oauth_account("a"), oauth_account("b")]);
        // Target b 429s between selection and commit.
        state.record_429(&id("b"), Some(Duration::from_secs(60)), now());
        let err = state
            .commit_switch(None, None, &id("b"), &params(), now())
            .unwrap_err();
        assert_eq!(
            err,
            SwitchError::TargetIneligible {
                account: id("b"),
                reason: IneligibleReason::CoolingDown,
            }
        );
        assert!(legacy(&state).is_none());
    }

    #[test]
    fn commit_switch_applies_on_clean_cas() {
        let mut state = PoolState::from_accounts(&[oauth_account("a"), oauth_account("b")]);
        state.current.insert(LEGACY_GROUP, id("a"));
        let current = legacy(&state);
        state
            .commit_switch(None, current.as_ref(), &id("b"), &params(), now())
            .unwrap();
        assert_eq!(legacy(&state), Some(id("b")));
    }

    #[test]
    fn fable_pick_does_not_move_nonfable_current() {
        // The whole point of the fable-head split: a Fable request that switches
        // accounts pins ONLY the fable_current slot and never disturbs the
        // non-Fable current. Non-Fable current is "a"; a Fable commit switches
        // to "b" — "a" must stay put while the fable slot becomes "b".
        let mut state = PoolState::from_accounts(&[oauth_account("a"), oauth_account("b")]);
        state.current.insert(LEGACY_GROUP, id("a"));
        // The fable slot starts empty, so the Fable CAS expects None (NOT "a") —
        // the non-Fable current does not block a first Fable pick.
        state
            .commit_switch_scoped(
                None,
                None,
                &id("b"),
                &params(),
                now(),
                select::RequestScope::Fable,
            )
            .expect("fable commit succeeds against the empty fable slot");
        assert_eq!(
            legacy(&state),
            Some(id("a")),
            "non-Fable current is untouched by a Fable switch"
        );
        assert_eq!(
            state.fable_current.get(&LEGACY_GROUP),
            Some(&id("b")),
            "the Fable switch pinned its own slot"
        );
    }

    #[test]
    fn nonfable_commit_does_not_move_fable_current() {
        // The mirror invariant: a non-Fable switch never disturbs the fable slot.
        let mut state = PoolState::from_accounts(&[oauth_account("a"), oauth_account("b")]);
        state.fable_current.insert(LEGACY_GROUP, id("a"));
        state
            .commit_switch(None, None, &id("b"), &params(), now())
            .expect("non-Fable commit succeeds against the empty non-Fable slot");
        assert_eq!(
            legacy(&state),
            Some(id("b")),
            "non-Fable switch pinned the non-Fable slot"
        );
        assert_eq!(
            state.fable_current.get(&LEGACY_GROUP),
            Some(&id("a")),
            "fable current is untouched by a non-Fable switch"
        );
    }

    #[test]
    fn evaluate_initial_selection_commits() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        let decision = pool.evaluate(None, &params(), now());
        assert_eq!(decision, Decision::Switch { to: id("a") });
        assert_eq!(snap_legacy(&pool), Some(id("a")));
    }

    #[test]
    fn evaluate_stays_then_switches_on_429() {
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        // a wins the initial id tiebreak.
        assert_eq!(
            pool.evaluate(None, &params(), now()),
            Decision::Switch { to: id("a") }
        );
        assert_eq!(pool.evaluate(None, &params(), now()), Decision::Stay);
        pool.record_429(&id("a"), Some(Duration::from_secs(120)), now());
        assert_eq!(
            pool.evaluate(None, &params(), now()),
            Decision::Switch { to: id("b") }
        );
        assert_eq!(snap_legacy(&pool), Some(id("b")));
    }

    #[test]
    fn evaluate_exhausted_clears_current_and_reports_reset() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        assert_eq!(
            pool.evaluate(None, &params(), now()),
            Decision::Switch { to: id("a") }
        );
        pool.record_429(&id("a"), Some(Duration::from_secs(2)), now());
        assert_eq!(
            pool.evaluate(None, &params(), now()),
            Decision::Exhausted {
                retry_after: Some(Duration::from_secs(2)),
            }
        );
        assert!(snap_legacy(&pool).is_none());
        assert!(pool.lease_for(None, &params()).is_err());
        // After the park expires the account is selectable again.
        assert_eq!(
            pool.evaluate(None, &params(), at(NOW_SECS + 3)),
            Decision::Switch { to: id("a") }
        );
    }

    #[test]
    fn switch_to_eligible_target_commits_and_refuses_ineligible() {
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        pool.evaluate(None, &params(), now());
        assert_eq!(snap_legacy(&pool), Some(id("a")));

        // Eligible target: manual switch commits.
        pool.switch_to(&id("b"), &params(), now()).unwrap();
        assert_eq!(snap_legacy(&pool), Some(id("b")));

        // Ineligible target (parked by a 429): refused, current unchanged.
        pool.record_429(&id("a"), Some(Duration::from_secs(60)), now());
        let err = pool.switch_to(&id("a"), &params(), now()).unwrap_err();
        assert_eq!(
            err,
            SwitchError::TargetIneligible {
                account: id("a"),
                reason: IneligibleReason::CoolingDown,
            }
        );
        assert_eq!(snap_legacy(&pool), Some(id("b")));

        // Unknown target: refused.
        let err = pool.switch_to(&id("ghost"), &params(), now()).unwrap_err();
        assert_eq!(err, SwitchError::UnknownAccount(id("ghost")));
    }

    #[test]
    fn switch_to_does_not_cancel_in_flight_leases() {
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        pool.evaluate(None, &params(), now());
        let lease = pool.lease_for(None, &params()).unwrap();
        assert_eq!(lease.account_id(), &id("a"));

        pool.switch_to(&id("b"), &params(), now()).unwrap();
        assert_eq!(snap_legacy(&pool), Some(id("b")));
        assert_eq!(
            pool.snapshot().accounts[0].in_flight,
            1,
            "manual switch leaves the live lease pinned to a"
        );
        drop(lease);
        assert_eq!(pool.snapshot().accounts[0].in_flight, 0);
    }

    #[test]
    fn lease_pins_credential_across_switch_and_refresh() {
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        pool.evaluate(None, &params(), now());
        let lease = pool.lease_for(None, &params()).unwrap();
        assert_eq!(lease.account_id(), &id("a"));
        assert_eq!(pool.snapshot().accounts[0].in_flight, 1);

        // Concurrent refresh + switch must not affect the live lease.
        pool.update_credential(
            &id("a"),
            AccountCredential::Oauth {
                account_uuid: "uuid-a".into(),
                access_token: "rotated".into(),
                refresh_token: "rotated".into(),
                expires_at_ms: 1,
                tier: None,
                last_refresh_ms: None,
            },
        );
        pool.record_429(&id("a"), Some(Duration::from_secs(60)), now());
        pool.evaluate(None, &params(), now());
        assert_eq!(snap_legacy(&pool), Some(id("b")));
        match lease.credential() {
            AccountCredential::Oauth { access_token, .. } => {
                assert_eq!(access_token, "at-a", "lease keeps the credential clone");
            }
            other => panic!("unexpected credential {other:?}"),
        }
        assert_eq!(
            pool.snapshot().accounts[0].in_flight,
            1,
            "switching away does not yank the lease"
        );
        drop(lease);
        assert_eq!(pool.snapshot().accounts[0].in_flight, 0);
    }

    #[test]
    fn lease_for_without_selection_reports_soonest_reset() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        pool.record_429(&id("a"), Some(Duration::from_secs(3600)), SystemTime::now());
        let err = pool.lease_for(None, &params()).unwrap_err();
        assert!(err.retry_after.is_some());
    }

    #[test]
    fn lease_for_refuses_cooling_current() {
        let pool = AccountPool::new(&[oauth_account("a")]);
        pool.evaluate(None, &params(), now());
        // Park far in the future relative to the real clock used by lease_for.
        pool.record_429(&id("a"), Some(Duration::from_secs(3600)), SystemTime::now());
        assert!(pool.lease_for(None, &params()).is_err());
    }

    #[test]
    fn reload_preserves_state_and_clears_removed_current() {
        let pool = AccountPool::new(&[oauth_account("a"), oauth_account("b")]);
        pool.evaluate(None, &params(), now());
        assert_eq!(snap_legacy(&pool), Some(id("a")));
        pool.record_429(&id("b"), Some(Duration::from_secs(60)), now());

        // Drop a, keep b (cooldown must survive), add c.
        pool.reload_accounts(&[oauth_account("b"), oauth_account("c")]);
        let snapshot = pool.snapshot();
        assert!(
            snapshot.legacy_current().is_none(),
            "removed current is cleared"
        );
        let b = snapshot.accounts.iter().find(|a| a.id == id("b")).unwrap();
        assert!(b.cooldown_until.is_some(), "surviving account keeps state");
        assert!(snapshot.accounts.iter().any(|a| a.id == id("c")));
        assert!(!snapshot.accounts.iter().any(|a| a.id == id("a")));
    }

    // ---- per-group sticky (routing enabled) ----

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

    #[test]
    fn evaluate_keeps_independent_current_per_group() {
        let pool = AccountPool::new(&[oauth_account("a"), codex_account("cx")]);
        // Selecting the claude group lands on the oauth account; selecting the
        // codex group lands on the codex account — independent slots.
        assert_eq!(
            pool.evaluate(Some(BackendGroup::Claude), &params(), now()),
            Decision::Switch { to: id("a") }
        );
        assert_eq!(
            pool.evaluate(Some(BackendGroup::Codex), &params(), now()),
            Decision::Switch { to: id("cx") }
        );
        let snapshot = pool.snapshot();
        assert_eq!(
            snapshot.current_for_group(BackendGroup::Claude),
            Some(&id("a"))
        );
        assert_eq!(
            snapshot.current_for_group(BackendGroup::Codex),
            Some(&id("cx"))
        );
    }

    #[test]
    fn group_filtered_evaluate_only_selects_in_group() {
        let pool = AccountPool::new(&[oauth_account("a"), codex_account("cx")]);
        // The codex group only ever selects the codex account, never the
        // claude one, even though it would win on id order.
        assert_eq!(
            pool.evaluate(Some(BackendGroup::Codex), &params(), now()),
            Decision::Switch { to: id("cx") }
        );
        // Lease for the codex group returns the codex account.
        let lease = pool
            .lease_for(Some(BackendGroup::Codex), &params())
            .unwrap();
        assert_eq!(lease.account_id(), &id("cx"));
    }

    #[test]
    fn empty_group_evaluates_to_exhausted() {
        // Only a claude account exists; the codex group has nothing.
        let pool = AccountPool::new(&[oauth_account("a")]);
        assert_eq!(
            pool.evaluate(Some(BackendGroup::Codex), &params(), now()),
            Decision::Exhausted { retry_after: None }
        );
        assert!(pool
            .lease_for(Some(BackendGroup::Codex), &params())
            .is_err());
        // The claude group is unaffected.
        assert_eq!(
            pool.evaluate(Some(BackendGroup::Claude), &params(), now()),
            Decision::Switch { to: id("a") }
        );
    }

    #[test]
    fn switch_to_sets_targets_own_group_slot() {
        let pool = AccountPool::new(&[oauth_account("a"), codex_account("cx")]);
        pool.evaluate(Some(BackendGroup::Claude), &params(), now());
        // Manually switching to the codex account sets the CODEX slot, leaving
        // the claude slot intact.
        pool.switch_to(&id("cx"), &params(), now()).unwrap();
        let snapshot = pool.snapshot();
        assert_eq!(
            snapshot.current_for_group(BackendGroup::Claude),
            Some(&id("a"))
        );
        assert_eq!(
            snapshot.current_for_group(BackendGroup::Codex),
            Some(&id("cx"))
        );
    }
}
