//! PURE selection: eligibility gates + soonest-7d-reset ranking.
//!
//! Everything here is a deterministic function of `(&PoolSnapshot, &SelectParams,
//! now)`. No IO, no clock reads, no locks — unit-test heavy by design. The
//! impure half (snapshotting, CAS commit) lives in `scheduler::AccountPool`.

use std::cmp::Ordering;
use std::time::{Duration, SystemTime};

use super::{AccountId, AccountSnapshot, CooldownSource, PoolSnapshot};
use crate::config::SchedulerConfig;
use crate::routing::BackendGroup;

/// Selection thresholds, derived from `SchedulerConfig`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectParams {
    /// 5h utilization ceiling (default 0.90).
    pub five_hour_max: f64,
    /// 7d utilization ceiling (default 0.99).
    pub seven_day_max: f64,
    /// Fable-weekly utilization ceiling for FABLE requests (default 0.98).
    pub fable_weekly_max: f64,
    /// Selection algorithm (config `scheduler.mode`).
    pub mode: crate::config::SchedulerMode,
    /// Usage data older than this makes an account ineligible — unless ALL
    /// accounts are stale, in which case selection falls back to
    /// headers-only mode (429-driven, the always-true path).
    pub usage_max_age: Duration,
}

impl From<&SchedulerConfig> for SelectParams {
    fn from(cfg: &SchedulerConfig) -> Self {
        Self {
            five_hour_max: cfg.five_hour_max,
            seven_day_max: cfg.seven_day_max,
            fable_weekly_max: cfg.fable_weekly_max,
            mode: cfg.mode,
            usage_max_age: Duration::from_secs(cfg.usage_max_age_secs),
        }
    }
}

/// What the scheduler should do, as decided by `pick`.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Current account is still eligible — session stickiness, don't move.
    Stay,
    /// Switch to `to` (CAS-committed against the snapshot's `current`).
    Switch { to: AccountId },
    /// Every account is ineligible. `retry_after` is the time until the
    /// soonest window reset, for the client-facing 429.
    Exhausted { retry_after: Option<Duration> },
}

/// Why an account was skipped — surfaced in `/llmux/status` and the TUI
/// so operators can see WHY the scheduler did what it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IneligibleReason {
    AuthUnhealthy,
    /// Operator pause (config `paused_accounts`): absolute — gates automatic
    /// selection, manual switch, and every degraded mode alike, until resumed.
    Paused,
    CoolingDown,
    FiveHourOverThreshold,
    SevenDayOverThreshold,
    UsageStale,
    /// A Fable request only: the account holds a live Fable-scoped cooldown
    /// (fable-usage W2). Non-Fable requests never see this.
    FableCoolingDown,
    /// A Fable request only: the account's Fable weekly bucket is currently
    /// constraining (preemptive Fable-critical avoidance, W2 point 4).
    /// Non-Fable requests never see this.
    FableWeeklyExhausted,
}

impl IneligibleReason {
    /// Whether the reason is a REAL failure — one that even a manual
    /// operator pin (issue #122) must respect: auth failure, operator
    /// pause, an account-wide 429/park, a Fable-scoped 429/park. The
    /// remaining reasons (ceiling thresholds, staleness, the preemptive
    /// Fable-weekly exclusion) are ADVISORY: an operator who manually
    /// switched just overrode them ("send it until it actually errors").
    pub fn hard_failure(self) -> bool {
        matches!(
            self,
            IneligibleReason::AuthUnhealthy
                | IneligibleReason::Paused
                | IneligibleReason::CoolingDown
                | IneligibleReason::FableCoolingDown
        )
    }
}

/// The Fable-scope classification of an inbound request (fable-usage W2). The
/// selector threads this so a Fable request is additionally gated by an
/// account's Fable-scoped cooldown / preemptive Fable exclusion, while a
/// non-Fable request IGNORES that state (identical to pre-W2 behavior — the
/// default for every display / status / degraded-mode caller too).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestScope {
    /// Not a Fable-family request — ignores all Fable-scoped state.
    NonFable,
    /// A Fable-family request — also gated by Fable cooldown + exclusion.
    Fable,
}

/// Single-account eligibility gate (FR3 step 1). Returns the first failing
/// gate, or `None` if eligible. `headers_only` is the all-stale fallback
/// mode in which staleness stops gating.
///
/// Boundary semantics: a window AT the threshold is still eligible
/// (`utilization <= max`); only strictly-over crosses the gate. A missing
/// window is a cold account — utilization 0, immediately eligible.
/// The 5h/7d/Fable ceilings that apply to THIS account: the per-account
/// override (config `account_limits`) when set, else the global params.
pub fn effective_limits(account: &AccountSnapshot, params: &SelectParams) -> (f64, f64, f64) {
    (
        account.limits.five_hour_max.unwrap_or(params.five_hour_max),
        account.limits.seven_day_max.unwrap_or(params.seven_day_max),
        account
            .limits
            .fable_weekly_max
            .unwrap_or(params.fable_weekly_max),
    )
}

pub fn eligibility(
    account: &AccountSnapshot,
    params: &SelectParams,
    now: SystemTime,
    headers_only: bool,
) -> Option<IneligibleReason> {
    gate(account, params, now, headers_only, false)
}

/// The full eligibility gate, parameterized by both degraded modes:
/// `headers_only` drops the staleness gate (all-stale fallback), and
/// `heuristic_degraded` drops the cooldown gate for accounts parked SOLELY by
/// a [`CooldownSource::Heuristic`] cooldown (retry-after-less 429 lockout
/// fallback). RetryAfter parks, auth failure, and the 5h/7d quota ceilings
/// STILL gate in either mode — only the transient heuristic guess is bypassed.
///
/// `eligibility` is the public, non-degraded-cooldown form (`heuristic_degraded
/// = false`) used everywhere display/status care about the real cooldown; the
/// selection path ([`pick`], `commit_switch`, `lease_for`) calls this directly
/// with the degraded flag when [`heuristic_degraded_mode`] holds.
pub fn gate(
    account: &AccountSnapshot,
    params: &SelectParams,
    now: SystemTime,
    headers_only: bool,
    heuristic_degraded: bool,
) -> Option<IneligibleReason> {
    if !account.healthy {
        return Some(IneligibleReason::AuthUnhealthy);
    }
    // Operator pause is absolute: it gates in every mode (including the
    // headers-only and heuristic-degraded fallbacks) — a paused account only
    // returns to service when the operator resumes it.
    if account.paused {
        return Some(IneligibleReason::Paused);
    }
    // A cooldown gates UNLESS we are in heuristic-degraded mode and this park
    // is a Heuristic one — a RetryAfter park is an explicit upstream
    // instruction and always gates.
    if account.cooldown_until.is_some_and(|until| until > now)
        && !(heuristic_degraded && account.cooldown_source == Some(CooldownSource::Heuristic))
    {
        return Some(IneligibleReason::CoolingDown);
    }
    let (five_max, seven_max, _) = effective_limits(account, params);
    let five = account
        .five_hour
        .map_or(0.0, |w| w.effective_utilization(now));
    if five > five_max {
        return Some(IneligibleReason::FiveHourOverThreshold);
    }
    let seven = account
        .seven_day
        .map_or(0.0, |w| w.effective_utilization(now));
    if seven > seven_max {
        return Some(IneligibleReason::SevenDayOverThreshold);
    }
    // Codex accounts are exempt from the staleness gate: there is no usage
    // poller for codex (usage.rs polls oauth only) — `x-codex-*` response
    // headers are their ONLY window source and are legitimately old between
    // requests. Gating on age would park an idle codex account forever.
    let staleness_applies = account.credential_kind != "codex";
    if !headers_only && staleness_applies && usage_is_stale(account, now, params.usage_max_age) {
        return Some(IneligibleReason::UsageStale);
    }
    None
}

/// Scope-aware eligibility gate (fable-usage W2, the core U8 mechanism). Layers
/// the Fable-scope checks ON TOP of the account-wide [`gate`], so:
///
/// - The account-wide gate runs first and unchanged: no AccountWide cooldown,
///   auth-healthy, under the 5h/7d ceilings, not stale.
/// - THEN, for a [`RequestScope::Fable`] request ONLY, the account is also
///   refused if it holds a live Fable-scoped cooldown ([`IneligibleReason::
///   FableCoolingDown`]) or its Fable weekly bucket is preemptively
///   constraining ([`IneligibleReason::FableWeeklyExhausted`]).
/// - A [`RequestScope::NonFable`] request is byte-for-byte the old [`gate`]:
///   Fable-scoped state is IGNORED, so a Fable-exhausted account still serves
///   non-Fable traffic — the whole point of U8.
pub fn gate_scoped(
    account: &AccountSnapshot,
    params: &SelectParams,
    now: SystemTime,
    headers_only: bool,
    heuristic_degraded: bool,
    scope: RequestScope,
) -> Option<IneligibleReason> {
    if let Some(reason) = gate(account, params, now, headers_only, heuristic_degraded) {
        return Some(reason);
    }
    if scope == RequestScope::Fable {
        // The Fable-scoped cooldown is a distinct park from the account-wide
        // one and is NOT bypassed by heuristic-degraded mode (that mode is about
        // account-wide transient 429 lockouts, a different mechanism).
        if account.fable_cooldown_active(now) {
            return Some(IneligibleReason::FableCoolingDown);
        }
        if account.fable_weekly_exhausted(now, effective_limits(account, params).2) {
            return Some(IneligibleReason::FableWeeklyExhausted);
        }
    }
    None
}

/// True when the account's freshest LIVE (non-expired) window observation is
/// older than `max_age`. An account with no live observations at all is cold,
/// not stale — expired windows carry no constraint and old-but-expired data
/// degrades to cold rather than blocking the account.
fn usage_is_stale(account: &AccountSnapshot, now: SystemTime, max_age: Duration) -> bool {
    match freshest_live_fetch(account, now) {
        Some(fetched_at) => now
            .duration_since(fetched_at)
            .is_ok_and(|age| age > max_age),
        None => false,
    }
}

/// Freshest `fetched_at` among the account's live (non-expired) windows —
/// the timestamp staleness is judged against, also surfaced as "usage stale
/// Xm" in blocking reasons.
pub fn freshest_live_fetch(account: &AccountSnapshot, now: SystemTime) -> Option<SystemTime> {
    [account.five_hour, account.seven_day]
        .into_iter()
        .flatten()
        .filter(|w| !w.is_expired(now))
        .map(|w| w.fetched_at)
        .max()
}

/// Headers-only degraded mode (FR3 staleness fallback): active when NO
/// account passes the full gate but at least one fails ONLY on staleness.
/// Then the staleness gate is dropped and scheduling falls back to
/// 429-driven behavior — better to try a stale account (upstream will 429 if
/// it's actually exhausted) than to refuse service because the usage
/// endpoint is down.
///
/// `group` scopes the decision to one backend group when `Some` (routing on)
/// — staleness in the codex group must not flip the claude group into
/// headers-only mode and vice versa. `None` (routing off) considers every
/// account, exactly as before.
pub fn headers_only_mode(
    snapshot: &PoolSnapshot,
    params: &SelectParams,
    group: Option<BackendGroup>,
    now: SystemTime,
) -> bool {
    let mut any_eligible = false;
    let mut any_stale_only = false;
    for account in &snapshot.accounts {
        if !in_group(account, group) {
            continue;
        }
        match eligibility(account, params, now, false) {
            None => any_eligible = true,
            Some(IneligibleReason::UsageStale) => any_stale_only = true,
            Some(_) => {}
        }
    }
    !any_eligible && any_stale_only
}

/// Heuristic-degraded mode (transient-429 lockout fallback): active when, in
/// the group scope, NO account passes the full gate AND at least one account is
/// blocked SOLELY by a [`CooldownSource::Heuristic`] cooldown — i.e. it would
/// be eligible if that one heuristic park were dropped (healthy, not
/// RetryAfter-parked, 5h/7d under threshold, not stale).
///
/// A retry-after-less upstream 429 is a transient server-side blip, not the
/// account's quota; the heuristic park exists only to rotate off a momentarily
/// limited account. When a burst parks the WHOLE group that way, refusing all
/// service for the full park is worse than retrying the soonest-freed account
/// (upstream will 429 again if it is still limited, which the forward loop
/// surfaces as a prompt transient 502 rather than a hard pool lockout). So in
/// this mode the heuristic cooldown gate is dropped. RetryAfter parks,
/// AuthUnhealthy, and real-quota (5h/7d over threshold) STILL gate.
///
/// `group` scopes the decision exactly like [`headers_only_mode`]: a heuristic
/// lockout in one backend group must not flip another group into degraded mode.
pub fn heuristic_degraded_mode(
    snapshot: &PoolSnapshot,
    params: &SelectParams,
    group: Option<BackendGroup>,
    now: SystemTime,
) -> bool {
    let headers_only = headers_only_mode(snapshot, params, group, now);
    let mut any_eligible = false;
    let mut any_heuristic_only = false;
    for account in &snapshot.accounts {
        if !in_group(account, group) {
            continue;
        }
        if eligibility(account, params, now, headers_only).is_none() {
            any_eligible = true;
            continue;
        }
        // Blocked solely by a Heuristic cooldown? It must currently gate ONLY
        // on CoolingDown with a Heuristic source, and pass every other gate
        // once that cooldown is dropped (heuristic_degraded = true).
        let blocks_on_heuristic_cooldown = account.cooldown_source
            == Some(CooldownSource::Heuristic)
            && account.cooldown_until.is_some_and(|until| until > now)
            && gate(account, params, now, headers_only, true).is_none();
        if blocks_on_heuristic_cooldown {
            any_heuristic_only = true;
        }
    }
    !any_eligible && any_heuristic_only
}

/// Count of in-group accounts this request could lease IF the transient
/// heuristic cooldowns were ignored — i.e. how many accounts a retry-after-less
/// 429 burst can sweep before the pool is genuinely out of candidates. Counts
/// via [`gate_scoped`] with `heuristic_degraded = true`, so paused / auth-dead /
/// real-quota (5h/7d) / RetryAfter-parked accounts are naturally excluded (they
/// still gate in degraded mode) — only the Heuristic cooldown is looked through.
///
/// The forward loop uses this to detect "I have now 429-swept every candidate in
/// this request" and pace instead of hammer: [`heuristic_degraded_mode`] keeps
/// handing out leases THROUGH the heuristic cooldown gate, so for a non-Fable
/// burst the pool never reaches `Exhausted` and the #83 grace-park budget path is
/// unreachable (2026-07-13T23:01Z incident: one opus-4-8 request swept 8 accounts
/// 13× in ~8s and 502'd the client while the upstream burst cleared ~20-30s
/// later). `headers_only` mirrors what the degraded selection path sees.
pub fn degraded_candidate_count(
    snapshot: &PoolSnapshot,
    params: &SelectParams,
    group: Option<BackendGroup>,
    scope: RequestScope,
    now: SystemTime,
) -> usize {
    let headers_only = headers_only_mode(snapshot, params, group, now);
    snapshot
        .accounts
        .iter()
        .filter(|account| in_group(account, group))
        .filter(|account| gate_scoped(account, params, now, headers_only, true, scope).is_none())
        .count()
}

/// Whether an account is in the selection scope: every account when `group`
/// is `None` (routing off), only same-group accounts when `Some`.
fn in_group(account: &AccountSnapshot, group: Option<BackendGroup>) -> bool {
    group.is_none_or(|g| account.group == g)
}

/// THE selection algorithm (FR3): gate, then rank eligible accounts by
/// soonest 7d `resets_at` (use-it-or-lose-it), tiebreak lower 5h effective
/// utilization, then stable id order.
///
/// Stickiness contract: if the group's `current` is still eligible this
/// returns `Stay` even when another account would rank higher. A missing 5h
/// window means a cold account (utilization 0, immediately eligible).
///
/// `group` scopes selection to one backend group when `Some` (routing on):
/// only same-group accounts are eligible, stickiness keys off that group's
/// current slot, and the codex `tier`-last rank rule becomes a no-op (every
/// candidate is already in-group). `None` (routing off) keeps the legacy
/// behavior: all accounts considered, the single legacy current slot, codex
/// kept as the cross-group overflow tier.
pub fn pick(
    snapshot: &PoolSnapshot,
    params: &SelectParams,
    group: Option<BackendGroup>,
    now: SystemTime,
) -> Decision {
    pick_scoped(snapshot, params, group, now, RequestScope::NonFable)
}

/// Scope-aware [`pick`] (fable-usage W2): identical selection, but the
/// eligibility filter and the stickiness re-check use [`gate_scoped`] with the
/// request's [`RequestScope`], so a Fable request excludes Fable-cooling /
/// Fable-critical accounts (and switches off a Fable-dead sticky current) while
/// non-Fable selection is unchanged. `RequestScope::NonFable` reproduces the
/// pre-W2 [`pick`] exactly.
pub fn pick_scoped(
    snapshot: &PoolSnapshot,
    params: &SelectParams,
    group: Option<BackendGroup>,
    now: SystemTime,
    scope: RequestScope,
) -> Decision {
    let headers_only = headers_only_mode(snapshot, params, group, now);
    let heuristic_degraded = heuristic_degraded_mode(snapshot, params, group, now);
    let eligible: Vec<&AccountSnapshot> = snapshot
        .accounts
        .iter()
        .filter(|a| in_group(a, group))
        .filter(|a| gate_scoped(a, params, now, headers_only, heuristic_degraded, scope).is_none())
        .collect();

    // Manual operator pin (issue #122) outranks every automatic policy: a
    // manual switch is an informed decision, so while the pin is alive both
    // scopes stay on the chosen account. Deliberately NOT the full gate —
    // thresholds, staleness and the preemptive Fable-weekly exclusion do
    // not break a pin ("send it until it actually errors"); only a REAL
    // failure does: auth failure, operator pause, an account-wide cooldown
    // (a recorded 429 also clears the pin outright), or — for the Fable
    // lane — a Fable-scoped cooldown. A hard-blocked or lapsed pin falls
    // through to normal selection.
    if let Some(pin) = snapshot.manual_pin_for(group) {
        if pin.until > now {
            if let Some(acct) = snapshot
                .accounts
                .iter()
                .find(|a| a.id == pin.account && in_group(a, group))
            {
                let hard_blocked = !acct.healthy
                    || acct.paused
                    || acct.cooldown_until.is_some_and(|until| until > now)
                    || (scope == RequestScope::Fable && acct.fable_cooldown_active(now));
                if !hard_blocked {
                    return if group_current(snapshot, group, scope) == Some(&pin.account) {
                        Decision::Stay
                    } else {
                        Decision::Switch {
                            to: pin.account.clone(),
                        }
                    };
                }
            }
        }
    }

    // The Fable lane schedules like the main lane, but on the FABLE weekly
    // window (issues #121/#8-report): candidates are ranked by
    // [`fable_score`] (Fable headroom × Fable reset urgency — the exact
    // `account_score` shape with the 7d window swapped for the Fable
    // bucket), so a soon-resetting full Fable bucket is burned before a
    // far-reset one (use-it-or-lose-it). Stickiness anchors on the fable
    // slot, SEEDED from the account-wide current when the slot is empty —
    // so with no clearly-better candidate (SWITCH_MARGIN, same damping as
    // the main lane) fable traffic stays on the account the operator sees
    // as current, a COLD Fable bucket included, instead of splitting
    // prompt-cache locality across subscriptions. Round-robin keeps its
    // absolute-stickiness contract: an eligible anchor is never abandoned.
    if scope == RequestScope::Fable && !heuristic_degraded {
        let anchor_id = group_current(snapshot, group, RequestScope::Fable)
            .or_else(|| group_current(snapshot, group, RequestScope::NonFable));
        let anchor = anchor_id.and_then(|id| eligible.iter().copied().find(|a| &a.id == id));
        let best = eligible
            .iter()
            .copied()
            .min_by(|a, b| fable_ranked(a, b, params, group, now));
        let chosen = match (anchor, best) {
            (Some(anchor), Some(best)) => {
                // A COLD challenger can never be "clearly better" (review
                // M2): with no live Fable window it scores a phantom FULL
                // bucket at urgency 1.0, which would out-score a half-used
                // far-reset anchor — hunting an evidence-free account splits
                // prompt-cache locality for nothing. Cold is a valid ANCHOR
                // (seed/stay) and a valid fallback when the anchor is
                // ineligible, but a proactive switch requires live evidence.
                let best_has_live_evidence =
                    live_reset(&best.fable_weekly().map(|s| s.window), now).is_some();
                let clearly_better = params.mode != crate::config::SchedulerMode::RoundRobin
                    && best.id != anchor.id
                    && best_has_live_evidence
                    && fable_score(best, params, now)
                        > fable_score(anchor, params, now) * (1.0 + SWITCH_MARGIN);
                if clearly_better {
                    best
                } else {
                    anchor
                }
            }
            (None, Some(best)) => best,
            (_, None) => {
                return Decision::Exhausted {
                    retry_after: soonest_reset(snapshot, now),
                }
            }
        };
        // `Stay` only when the FABLE slot itself already points at the
        // choice — a seeded/fallback anchor must materialize as a Switch so
        // the commit writes `fable_current` and the lease fast-path works.
        return if group_current(snapshot, group, RequestScope::Fable) == Some(&chosen.id) {
            Decision::Stay
        } else {
            Decision::Switch {
                to: chosen.id.clone(),
            }
        };
    }

    // In heuristic-degraded mode every candidate is heuristic-parked: rank by
    // soonest `cooldown_until` so the next request lands on the soonest-freed
    // account. Otherwise use the mode's comparator: round-robin picks the next
    // ROSTER-order account after the current (wrapping) — deterministic,
    // score-free — while default uses the perishability comparator.
    let best = if params.mode == crate::config::SchedulerMode::RoundRobin && !heuristic_degraded {
        round_robin_next(snapshot, &eligible, group_current(snapshot, group, scope))
    } else {
        eligible
            .iter()
            .copied()
            .min_by(|a, b| ranked(a, b, params, group, now, heuristic_degraded))
    };

    // Stickiness with a perishability override: stay on an eligible current
    // unless some account is worth CLEARLY more right now — its score exceeds
    // the current's by SWITCH_MARGIN. This proactively burns soon-to-reset
    // quota instead of camping an account with a long weekly runway, while the
    // margin (plus the 60s evaluation cadence) damps cache-thrashing hand-offs
    // between near-equal accounts. A switch off an *ineligible* current is
    // handled below (current is not in `eligible`, so this block is skipped).
    // Stickiness does not apply in heuristic-degraded mode: every candidate is
    // parked, so `account_score` carries no usable-burst signal — honor the
    // soonest-`cooldown_until` choice instead of camping the (also-parked)
    // current.
    if !heuristic_degraded {
        if let Some(current_id) = group_current(snapshot, group, scope) {
            if eligible.iter().any(|a| &a.id == current_id) {
                // Round-robin stickiness is ABSOLUTE: an eligible current is
                // never proactively abandoned — the whole point of the mode is
                // to preserve upstream prompt-cache locality until the account
                // is hard ineligible.
                if params.mode == crate::config::SchedulerMode::RoundRobin {
                    return Decision::Stay;
                }
                let current = eligible
                    .iter()
                    .copied()
                    .find(|a| &a.id == current_id)
                    .expect("checked above");
                let clearly_better = match best {
                    Some(best) if &best.id != current_id => {
                        account_score(best, params, now)
                            > account_score(current, params, now) * (1.0 + SWITCH_MARGIN)
                    }
                    _ => false,
                };
                if !clearly_better {
                    return Decision::Stay;
                }
            }
        }
    }

    match best {
        Some(best) => Decision::Switch {
            to: best.id.clone(),
        },
        None => Decision::Exhausted {
            retry_after: soonest_reset(snapshot, now),
        },
    }
}

/// Round-robin choice: the first ELIGIBLE account after the current one in
/// roster (snapshot/config) order, wrapping; with no current, the first
/// eligible account in roster order. `eligible` preserves roster order
/// because it filters `snapshot.accounts` in place.
fn round_robin_next<'a>(
    snapshot: &'a PoolSnapshot,
    eligible: &[&'a AccountSnapshot],
    current: Option<&AccountId>,
) -> Option<&'a AccountSnapshot> {
    let is_eligible = |id: &AccountId| eligible.iter().any(|a| &a.id == id);
    let start = current
        .and_then(|cur| snapshot.accounts.iter().position(|a| &a.id == cur))
        .map(|i| i + 1)
        .unwrap_or(0);
    let n = snapshot.accounts.len();
    (0..n)
        .map(|offset| &snapshot.accounts[(start + offset) % n])
        .find(|a| is_eligible(&a.id))
}

/// The current account for the active selection scope: the group's slot when
/// `Some`, the legacy slot when `None`.
fn group_current(
    snapshot: &PoolSnapshot,
    group: Option<BackendGroup>,
    scope: RequestScope,
) -> Option<&AccountId> {
    match group {
        Some(g) => snapshot.current_for_scope(g, scope),
        None => snapshot.legacy_current_scoped(scope),
    }
}

/// THE next-in-line account for a scope: the one `pick` would switch to if the
/// scope's current became ineligible — the best eligible candidate that is NOT
/// the current slot. `None` when the scope has no other eligible account. Pure,
/// reusing the same `eligibility` gate + `rank` comparator as [`pick`], so the
/// displayed "next" can never disagree with the selector. With `group` `Some`
/// the search is scoped to that backend group (per-group `next` lines).
pub fn next_in_line(
    snapshot: &PoolSnapshot,
    params: &SelectParams,
    now: SystemTime,
    group: Option<BackendGroup>,
) -> Option<AccountId> {
    let headers_only = headers_only_mode(snapshot, params, group, now);
    // The per-group "next" line is a display reader over the non-Fable current.
    let current = group_current(snapshot, group, RequestScope::NonFable);
    if params.mode == crate::config::SchedulerMode::RoundRobin {
        let eligible: Vec<&AccountSnapshot> = snapshot
            .accounts
            .iter()
            .filter(|a| in_group(a, group))
            .filter(|a| Some(&a.id) != current)
            .filter(|a| eligibility(a, params, now, headers_only).is_none())
            .collect();
        return round_robin_next(snapshot, &eligible, current).map(|a| a.id.clone());
    }
    snapshot
        .accounts
        .iter()
        .filter(|a| in_group(a, group))
        .filter(|a| Some(&a.id) != current)
        .filter(|a| eligibility(a, params, now, headers_only).is_none())
        .min_by(|a, b| rank(a, b, params, group, now))
        .map(|a| a.id.clone())
}

/// The weekly window length — the horizon over which 7d-reset perishability
/// ramps. An account whose 7d window resets a full `SEVEN_DAY_PERIOD` from now
/// (a just-started window) is maximally non-perishable (urgency 1.0); one whose
/// window resets imminently is maximally perishable (urgency [`URGENCY_MAX`]).
const SEVEN_DAY_PERIOD: Duration = Duration::from_secs(7 * 24 * 3600);

/// Cap on the perishability multiplier, so a near-reset account with little
/// usable burst can't dominate a healthy one. With `URGENCY_MAX = 4`, a
/// soonest-reset account wins over a full cold one only while it can still serve
/// at least ~`0.9 / 4 ≈ 0.22` of headroom — below that it is too gated to be
/// worth chasing and `servable_now` lets the usable account win.
const URGENCY_MAX: f64 = 4.0;

/// Relative score margin a non-current account must beat the current account by
/// before the scheduler proactively switches off an *eligible* current. Burning
/// soon-to-reset quota is worth a switch; a marginal or tiebreak-only difference
/// is not (it would only cost upstream prompt-cache locality). See
/// `.prd/09-scheduler-perishability.md`.
const SWITCH_MARGIN: f64 = 0.25;

/// The scheduler's value for an account — higher is preferred. Derivation in
/// `.prd/09-scheduler-perishability.md` (supersedes `07`).
///
/// `servable_now = min(5h headroom, 7d headroom)` is the work the account can
/// serve before it next gates on EITHER limit. The 5h window rate-caps the 7d
/// budget, so an account rich in weekly quota but near its 5h cap is worth
/// little right now, and one with a full 5h window but little weekly budget is
/// likewise capped — the binding limit wins.
///
/// `urgency` scales with how far an account is through its weekly window: budget
/// that resets soon refreshes regardless, so it is perishable and should be
/// burned first, while a long-runway account is preserved as a reservoir. The
/// ramp is linear across the whole `SEVEN_DAY_PERIOD` — a reset ~1 day out gets
/// ≈3.5×, ~6 days out ≈1.4×, and a just-reset / no-live-7d (cold) account 1.0×
/// (least perishable, used last). The boost is gated by `servable_now`: a maxed
/// 5h window or drained weekly budget drives it toward 0, so an
/// urgent-but-unusable account cannot win — it would gate immediately.
pub fn account_score(account: &AccountSnapshot, params: &SelectParams, now: SystemTime) -> f64 {
    let five = account
        .five_hour
        .map_or(0.0, |w| w.effective_utilization(now));
    let seven = account
        .seven_day
        .map_or(0.0, |w| w.effective_utilization(now));
    let (five_max, seven_max, _) = effective_limits(account, params);
    let r5 = (five_max - five).max(0.0);
    let r7 = (seven_max - seven).max(0.0);
    let servable_now = r5.min(r7);
    let urgency = match live_reset(&account.seven_day, now) {
        Some(reset) => {
            let secs_left = reset
                .duration_since(now)
                .unwrap_or(Duration::ZERO)
                .as_secs_f64();
            // Fraction of a full weekly window still on the clock: 1.0 = just
            // reset (not perishable), 0.0 = resets right now (most perishable).
            let frac_left = (secs_left / SEVEN_DAY_PERIOD.as_secs_f64()).clamp(0.0, 1.0);
            1.0 + (URGENCY_MAX - 1.0) * (1.0 - frac_left)
        }
        None => 1.0,
    };
    servable_now * urgency
}

/// [`account_score`] for the FABLE lane (issue #121 follow-up): the same
/// servable × urgency shape with the Fable weekly bucket taking the 7d
/// window's roles. `servable_now = min(5h, 7d, Fable headroom)` — a Fable
/// request burns all three budgets, so the binding one wins.
///
/// `urgency` is HYPERBOLIC (`week / time-left`, clamped), not the main
/// lane's linear ramp: the linear ramp under-discriminates near the reset
/// (38h-left vs 3h-left differ by ×1.19 — inside `SWITCH_MARGIN`, so the
/// live defect this fixes — a FULL bucket resetting in 3h ignored in favor
/// of a 93% bucket resetting in 1d14h — would survive a linear ramp). The
/// hyperbola is the required burn RATE: remaining budget over the time
/// left to burn it (3h-left ≈ 56×, 38h ≈ 4.4×, 6d ≈ 1.2×), so imminent
/// resets win decisively while far resets leave stickiness in charge. A
/// COLD bucket (never observed) has no known perishability and scores 1.0×
/// — held by anchoring, never hunted.
pub fn fable_score(account: &AccountSnapshot, params: &SelectParams, now: SystemTime) -> f64 {
    let five = account
        .five_hour
        .map_or(0.0, |w| w.effective_utilization(now));
    let seven = account
        .seven_day
        .map_or(0.0, |w| w.effective_utilization(now));
    let fable_window = account.fable_weekly().map(|s| s.window);
    let fable = fable_window.map_or(0.0, |w| w.effective_utilization(now));
    let (five_max, seven_max, fable_max) = effective_limits(account, params);
    let r5 = (five_max - five).max(0.0);
    let r7 = (seven_max - seven).max(0.0);
    let rf = (fable_max - fable).max(0.0);
    let servable_now = r5.min(r7).min(rf);
    let urgency = match live_reset(&fable_window, now) {
        Some(reset) => {
            let secs_left = reset
                .duration_since(now)
                .unwrap_or(Duration::ZERO)
                .as_secs_f64()
                .max(1.0);
            (SEVEN_DAY_PERIOD.as_secs_f64() / secs_left).clamp(1.0, FABLE_URGENCY_CAP)
        }
        None => 1.0,
    };
    servable_now * urgency
}

/// Ceiling for the hyperbolic Fable urgency: a reset ≤ ~100 minutes out is
/// already maximally urgent — an unbounded hyperbola near zero would just
/// amplify float noise between two both-imminent buckets.
const FABLE_URGENCY_CAP: f64 = 100.0;

/// [`rank`] for the FABLE lane: higher [`fable_score`] first, then soonest
/// live Fable reset (known before unknown), then lower 5h utilization, then
/// stable id. The codex tier rule is irrelevant here (fable models route to
/// the claude group).
fn fable_ranked(
    a: &AccountSnapshot,
    b: &AccountSnapshot,
    params: &SelectParams,
    _group: Option<BackendGroup>,
    now: SystemTime,
) -> Ordering {
    let fable_reset = |x: &AccountSnapshot| live_reset(&x.fable_weekly().map(|s| s.window), now);
    fable_score(b, params, now)
        .total_cmp(&fable_score(a, params, now))
        .then_with(|| match (fable_reset(a), fable_reset(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
        .then_with(|| {
            let five =
                |x: &AccountSnapshot| x.five_hour.map_or(0.0, |w| w.effective_utilization(now));
            five(a).total_cmp(&five(b))
        })
        .then_with(|| a.id.cmp(&b.id))
}

/// Ranking comparator: provider tier first (codex accounts are the overflow
/// pool — they have no Anthropic quota windows and must never be auto-picked
/// over a healthy anthropic account; manual TUI switch still works), then
/// **higher [`account_score`]** (usable burst now × weekly-quota perishability,
/// so among usable accounts the soonest-to-reset is preferred and a long-runway
/// account is preserved), then lower 5h effective utilization, then soonest 7d
/// reset as a deep tiebreak (known reset before unknown), then stable id.
///
/// Under a group filter (`group.is_some()`, routing on) the codex `tier`-last
/// rule is a NO-OP: every candidate is already in the same group, so there is
/// no cross-group overflow to demote. It is kept for the `None` (legacy) path
/// where codex is the cross-group overflow tier.
/// Ranking comparator selector: in heuristic-degraded mode every candidate is
/// parked, so rank by soonest `cooldown_until` (the soonest-freed account is
/// served first, per the lockout-recovery contract), tiebreaking on stable id.
/// Otherwise defer to the normal [`rank`] perishability comparator.
fn ranked(
    a: &AccountSnapshot,
    b: &AccountSnapshot,
    params: &SelectParams,
    group: Option<BackendGroup>,
    now: SystemTime,
    heuristic_degraded: bool,
) -> Ordering {
    if heuristic_degraded {
        // `None` cooldown_until sorts last (an account with no park should not
        // be picked over a soon-to-free one, though in practice degraded mode
        // implies every candidate is parked).
        let key = |x: &AccountSnapshot| {
            x.cooldown_until
                .unwrap_or(SystemTime::UNIX_EPOCH + Duration::from_secs(u64::MAX / 2))
        };
        return key(a).cmp(&key(b)).then_with(|| a.id.cmp(&b.id));
    }
    rank(a, b, params, group, now)
}

fn rank(
    a: &AccountSnapshot,
    b: &AccountSnapshot,
    params: &SelectParams,
    group: Option<BackendGroup>,
    now: SystemTime,
) -> Ordering {
    if group.is_none() {
        let tier = |x: &AccountSnapshot| u8::from(x.credential_kind == "codex");
        let by_tier = tier(a).cmp(&tier(b));
        if by_tier != Ordering::Equal {
            return by_tier;
        }
    }
    // Higher score first (descending).
    account_score(b, params, now)
        .total_cmp(&account_score(a, params, now))
        .then_with(|| {
            let five_a = a.five_hour.map_or(0.0, |w| w.effective_utilization(now));
            let five_b = b.five_hour.map_or(0.0, |w| w.effective_utilization(now));
            five_a.total_cmp(&five_b)
        })
        .then_with(
            || match (live_reset(&a.seven_day, now), live_reset(&b.seven_day, now)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
        )
        .then_with(|| a.id.cmp(&b.id))
}

fn live_reset(window: &Option<super::window::QuotaWindow>, now: SystemTime) -> Option<SystemTime> {
    window.filter(|w| !w.is_expired(now)).map(|w| w.resets_at)
}

/// THE display order (B1): indices into `snapshot.accounts`, sorted by the
/// scheduler's actual preference —
///
/// 1. the currently selected account first (even if it just became
///    ineligible: it is still serving leases until the next evaluation),
/// 2. then every eligible account in [`rank`] order (the literal order the
///    scheduler would pick them next),
/// 3. then ineligible accounts, grouped last, in stable config order.
///
/// Pure over the same inputs as [`pick`] and reusing the same `eligibility`
/// gate + `rank` comparator, so the TUI / status output can never disagree
/// with the selector.
pub fn selection_order(
    snapshot: &PoolSnapshot,
    params: &SelectParams,
    now: SystemTime,
) -> Vec<usize> {
    // The display lists ALL accounts (across groups), so it ranks with the
    // legacy comparator (codex overflow last) and treats an account as
    // "current" if it is the current pick for ANY group.
    let group = None;
    let headers_only = headers_only_mode(snapshot, params, group, now);
    let mut current: Vec<usize> = Vec::new();
    let mut eligible: Vec<usize> = Vec::new();
    let mut ineligible: Vec<usize> = Vec::new();
    for (idx, account) in snapshot.accounts.iter().enumerate() {
        if snapshot.is_current(&account.id) {
            current.push(idx);
        } else if eligibility(account, params, now, headers_only).is_none() {
            eligible.push(idx);
        } else {
            ineligible.push(idx);
        }
    }
    if params.mode == crate::config::SchedulerMode::RoundRobin {
        // Display the literal rotation: roster order starting after the
        // current account (wrapping) — the order round_robin_next serves.
        let start = current.first().map(|&i| i + 1).unwrap_or(0);
        let n = snapshot.accounts.len().max(1);
        eligible.sort_by_key(|&i| (i + n - (start % n)) % n);
    } else {
        eligible.sort_by(|&a, &b| {
            rank(
                &snapshot.accounts[a],
                &snapshot.accounts[b],
                params,
                group,
                now,
            )
        });
    }
    current
        .into_iter()
        .chain(eligible)
        .chain(ineligible)
        .collect()
}

/// Human-readable blocking reason for an ineligible account, with the
/// concrete numbers an operator acts on: "cooldown 3m12s",
/// "7d 99.4% > 99%", "usage stale 14m03s", "auth failed". Shared by the TUI
/// status column and `/llmux/status` so the wording never drifts.
pub fn blocking_reason(
    account: &AccountSnapshot,
    reason: IneligibleReason,
    params: &SelectParams,
    now: SystemTime,
) -> String {
    match reason {
        IneligibleReason::AuthUnhealthy => "auth failed".to_string(),
        IneligibleReason::Paused => "paused".to_string(),
        IneligibleReason::CoolingDown => {
            match account
                .cooldown_until
                .and_then(|until| until.duration_since(now).ok())
            {
                Some(left) => format!("cooldown {}", compact_duration(left)),
                None => "cooldown".to_string(),
            }
        }
        IneligibleReason::FiveHourOverThreshold => {
            let util = account
                .five_hour
                .map_or(0.0, |w| w.effective_utilization(now));
            format!(
                "5h {:.1}% > {:.0}%",
                util * 100.0,
                effective_limits(account, params).0 * 100.0
            )
        }
        IneligibleReason::SevenDayOverThreshold => {
            let util = account
                .seven_day
                .map_or(0.0, |w| w.effective_utilization(now));
            format!(
                "7d {:.1}% > {:.0}%",
                util * 100.0,
                effective_limits(account, params).1 * 100.0
            )
        }
        IneligibleReason::UsageStale => {
            match freshest_live_fetch(account, now).and_then(|at| now.duration_since(at).ok()) {
                Some(age) => format!("usage stale {}", compact_duration(age)),
                None => "usage stale".to_string(),
            }
        }
        IneligibleReason::FableCoolingDown => {
            match account
                .fable_cooldown_until(now)
                .and_then(|until| until.duration_since(now).ok())
            {
                Some(left) => format!("fable cooldown {}", compact_duration(left)),
                None => "fable cooldown".to_string(),
            }
        }
        IneligibleReason::FableWeeklyExhausted => match account.fable_weekly() {
            Some(fable) => format!(
                "fable {:.0}% critical",
                fable.window.effective_utilization(now) * 100.0
            ),
            None => "fable critical".to_string(),
        },
    }
}

/// Compact no-space duration: "2d4h", "6h52m", "3m12s", "42s". Used in
/// blocking reasons and the dashboard's countdown cells.
pub fn compact_duration(d: Duration) -> String {
    let total = d.as_secs();
    let (days, hours, mins, secs) = (
        total / 86_400,
        (total % 86_400) / 3_600,
        (total % 3_600) / 60,
        total % 60,
    );
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{mins:02}m")
    } else if mins > 0 {
        format!("{mins}m{secs:02}s")
    } else {
        format!("{secs}s")
    }
}

/// Time until the soonest moment any account could become usable: the
/// minimum over all future window `resets_at` AND future `cooldown_until`
/// across auth-healthy accounts — the `retry-after` answer when exhausted.
/// (A 2s 429-park is sooner than any window reset and must win.)
pub fn soonest_reset(snapshot: &PoolSnapshot, now: SystemTime) -> Option<Duration> {
    snapshot
        .accounts
        .iter()
        .filter(|a| a.healthy)
        .flat_map(|a| {
            a.five_hour
                .map(|w| w.resets_at)
                .into_iter()
                .chain(a.seven_day.map(|w| w.resets_at))
                .chain(a.cooldown_until)
        })
        .filter_map(|t| t.duration_since(now).ok())
        .filter(|d| *d > Duration::ZERO)
        .min()
}

/// Why a (group, scope) pool has no eligible account (issue #71). Every
/// blocker being a cooldown means the pool recovers in seconds — the proxy
/// must answer with a prompt-retry signal, never a window-reset-scale
/// `retry-after`. Anything else is a genuine quota/health exhaustion for
/// which the window-reset answer is honest.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExhaustionKind {
    /// Every otherwise-usable in-group account is parked ONLY by an
    /// account-wide or requested-scope cooldown; `min_expiry` is the soonest
    /// recovery across those parks. `upstream_mandated` is true when that
    /// soonest park came from an upstream `retry-after`
    /// ([`CooldownSource::RetryAfter`]) — a wait upstream itself dictated, so
    /// a 429 carrying it is honest. False means the soonest park is our own
    /// heuristic guess on a header-less 429 (or a model-scoped park), i.e. a
    /// transient the client should just retry through.
    CooldownBlocked {
        min_expiry: Duration,
        upstream_mandated: bool,
    },
    /// At least part of the pool is gated by quota windows / auth health /
    /// staleness — not a transient park.
    WindowBlocked,
}

/// Classify WHY the (group, scope) pool is exhausted right now, plus how many
/// accounts the answer speaks for (the honest count for the client-facing
/// message — never the whole multi-group pool size).
///
/// - Any account blocked only by `CoolingDown`/`FableCoolingDown` counts as
///   transiently parked; if at least one such account exists the pool is
///   [`ExhaustionKind::CooldownBlocked`] (it recovers when the soonest park
///   lapses, regardless of how the rest is gated).
/// - Otherwise [`ExhaustionKind::WindowBlocked`], counting every in-group
///   account.
pub fn classify_exhaustion(
    snapshot: &PoolSnapshot,
    params: &SelectParams,
    group: Option<BackendGroup>,
    scope: RequestScope,
    now: SystemTime,
) -> (ExhaustionKind, usize) {
    let headers_only = headers_only_mode(snapshot, params, group, now);
    let heuristic_degraded = heuristic_degraded_mode(snapshot, params, group, now);
    let mut in_group_total = 0usize;
    let mut cooldown_blocked = 0usize;
    // (expiry, came-from-upstream-retry-after) of the soonest park seen.
    let mut min_park: Option<(Duration, bool)> = None;
    for a in snapshot.accounts.iter().filter(|a| in_group(a, group)) {
        in_group_total += 1;
        match gate_scoped(a, params, now, headers_only, heuristic_degraded, scope) {
            Some(IneligibleReason::CoolingDown) | Some(IneligibleReason::FableCoolingDown) => {
                cooldown_blocked += 1;
                let mandated = matches!(
                    a.cooldown_source,
                    Some(crate::scheduler::CooldownSource::RetryAfter)
                );
                let account_wide = a
                    .cooldown_until
                    .and_then(|t| t.duration_since(now).ok())
                    .map(|d| (d, mandated));
                // Model-scoped parks are always our own header-less-429
                // heuristic — never upstream-mandated.
                let scoped = a
                    .scoped_cooldowns
                    .iter()
                    .filter(|c| match scope {
                        RequestScope::Fable => {
                            c.scope.matches_label(crate::routing::FABLE_SCOPE_LABEL)
                        }
                        _ => false,
                    })
                    .filter_map(|c| c.until.duration_since(now).ok())
                    .min()
                    .map(|d| (d, false));
                for cand in account_wide.into_iter().chain(scoped) {
                    min_park = Some(match min_park {
                        Some(cur) if cur.0 <= cand.0 => cur,
                        _ => cand,
                    });
                }
            }
            Some(_) | None => {}
        }
    }
    if cooldown_blocked > 0 {
        // A gate can say CoolingDown while the snapshot's timestamps have
        // already lapsed mid-race; fall back to the heuristic park length so
        // the caller still answers seconds, not window resets.
        let (min_expiry, upstream_mandated) =
            min_park.unwrap_or((crate::scheduler::DEFAULT_HEURISTIC_COOLDOWN, false));
        (
            ExhaustionKind::CooldownBlocked {
                min_expiry,
                upstream_mandated,
            },
            cooldown_blocked,
        )
    } else {
        (ExhaustionKind::WindowBlocked, in_group_total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::window::{LimitSeverity, QuotaWindow, ScopedQuotaWindow, WindowSource};
    use crate::scheduler::{Cooldown429Reason, CooldownScope, CooldownSource, ModelScopedCooldown};

    const HOUR: u64 = 3600;
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

    /// Fresh window observation (fetched "now", resets in the future).
    fn window(utilization: f64, resets_in_secs: u64) -> QuotaWindow {
        window_fetched(utilization, resets_in_secs, NOW_SECS)
    }

    fn window_fetched(utilization: f64, resets_in_secs: u64, fetched_secs: u64) -> QuotaWindow {
        QuotaWindow {
            utilization,
            resets_at: at(NOW_SECS + resets_in_secs),
            fetched_at: at(fetched_secs),
            source: WindowSource::UsagePoll,
        }
    }

    fn account(id: &str) -> AccountSnapshot {
        AccountSnapshot {
            id: AccountId(id.to_string()),
            healthy: true,
            credential_kind: "oauth",
            group: BackendGroup::Claude,
            five_hour: None,
            seven_day: None,
            scoped_limits: Vec::new(),
            scoped_cooldowns: Vec::new(),
            cooldown_until: None,
            cooldown_source: None,
            in_flight: 0,
            token_expires_at_ms: None,
            last_refresh_ms: None,
            paused: false,
            limits: crate::config::AccountLimits::default(),
        }
    }

    #[test]
    fn round_robin_stays_until_hard_ineligible_then_takes_roster_order() {
        let rr = || {
            let mut p = params();
            p.mode = crate::config::SchedulerMode::RoundRobin;
            p
        };
        // Three accounts in roster order a, b, c; current = b.
        let mut a = account("a");
        a.seven_day = Some(window(0.10, 600_000));
        let mut b = account("b");
        b.seven_day = Some(window(0.95, 1_000)); // resets soon → default mode would prefer burning it? b IS current
        let mut c = account("c");
        c.seven_day = Some(window(0.20, 500_000));
        // Current b is still eligible → absolute stay, even though other
        // accounts may score higher.
        let snap = pool(vec![a.clone(), b.clone(), c.clone()], Some("b"));
        assert_eq!(pick(&snap, &rr(), None, now()), Decision::Stay);
        // Current b crosses its 7d ceiling → hard ineligible → the NEXT
        // roster account after b is c (wrap would continue to a).
        let mut b_dead = b.clone();
        b_dead.seven_day = Some(window(0.995, 1_000));
        let snap = pool(vec![a.clone(), b_dead, c.clone()], Some("b"));
        assert_eq!(
            pick(&snap, &rr(), None, now()),
            Decision::Switch {
                to: AccountId("c".into())
            },
            "roster order after the current, not the best score"
        );
        // Wrapping: current = c (last) → next is a.
        let mut c_dead = c.clone();
        c_dead.seven_day = Some(window(0.995, 1_000));
        let snap = pool(vec![a, account("b2"), c_dead], Some("c"));
        assert_eq!(
            pick(&snap, &rr(), None, now()),
            Decision::Switch {
                to: AccountId("a".into())
            },
            "rotation wraps to the roster head"
        );
    }

    #[test]
    fn per_account_limit_overrides_beat_the_global_ceilings() {
        // Global 5h ceiling is 0.90; this account overrides it down to 0.50.
        let mut a = account("alpha");
        a.five_hour = Some(window(0.60, 3_600));
        a.limits.five_hour_max = Some(0.50);
        assert_eq!(
            eligibility(&a, &params(), now(), false),
            Some(IneligibleReason::FiveHourOverThreshold),
            "0.60 > per-account 0.50 even though the global ceiling is 0.90"
        );
        // The blocking reason reports the EFFECTIVE ceiling.
        let reason = blocking_reason(
            &a,
            IneligibleReason::FiveHourOverThreshold,
            &params(),
            now(),
        );
        assert!(
            reason.contains("> 50%"),
            "effective ceiling in text: {reason}"
        );
        // Clearing the override restores the global ceiling.
        a.limits.five_hour_max = None;
        assert_eq!(eligibility(&a, &params(), now(), false), None);
    }

    #[test]
    fn paused_account_is_ineligible_in_every_mode_and_reads_paused() {
        let mut a = account("alpha");
        a.paused = true;
        // Normal, headers-only, and heuristic-degraded modes all refuse it.
        for (headers_only, degraded) in [(false, false), (true, false), (false, true), (true, true)]
        {
            assert_eq!(
                gate(&a, &params(), now(), headers_only, degraded),
                Some(IneligibleReason::Paused),
                "headers_only={headers_only} degraded={degraded}"
            );
        }
        assert_eq!(
            blocking_reason(&a, IneligibleReason::Paused, &params(), now()),
            "paused"
        );
        // Resumed → eligible again.
        a.paused = false;
        assert_eq!(eligibility(&a, &params(), now(), false), None);
    }

    /// Build a snapshot whose legacy current slot is `current` (the
    /// routing-disabled path these tests exercise).
    fn pool(accounts: Vec<AccountSnapshot>, current: Option<&str>) -> PoolSnapshot {
        let mut map = std::collections::BTreeMap::new();
        if let Some(c) = current {
            map.insert(BackendGroup::Claude, AccountId(c.to_string()));
        }
        PoolSnapshot {
            accounts,
            current: map,
            fable_current: std::collections::BTreeMap::new(),
            manual_pin: std::collections::BTreeMap::new(),
        }
    }

    fn id(s: &str) -> AccountId {
        AccountId(s.to_string())
    }

    // ---- eligibility gates ----

    #[test]
    fn cold_account_is_eligible() {
        assert_eq!(eligibility(&account("a"), &params(), now(), false), None);
    }

    #[test]
    fn five_hour_threshold_boundary() {
        let mut a = account("a");
        a.five_hour = Some(window(0.90, HOUR));
        assert_eq!(
            eligibility(&a, &params(), now(), false),
            None,
            "exactly at threshold is eligible"
        );
        a.five_hour = Some(window(0.9000001, HOUR));
        assert_eq!(
            eligibility(&a, &params(), now(), false),
            Some(IneligibleReason::FiveHourOverThreshold),
            "just over threshold is ineligible"
        );
    }

    #[test]
    fn seven_day_threshold_boundary() {
        let mut a = account("a");
        a.seven_day = Some(window(0.99, 24 * HOUR));
        assert_eq!(eligibility(&a, &params(), now(), false), None);
        a.seven_day = Some(window(0.991, 24 * HOUR));
        assert_eq!(
            eligibility(&a, &params(), now(), false),
            Some(IneligibleReason::SevenDayOverThreshold)
        );
    }

    #[test]
    fn expired_window_reads_as_cold() {
        let mut a = account("a");
        // 100% utilized, but the reset timestamp already passed.
        a.five_hour = Some(QuotaWindow {
            utilization: 1.0,
            resets_at: at(NOW_SECS - 1),
            fetched_at: at(NOW_SECS - 10),
            source: WindowSource::Headers,
        });
        assert_eq!(eligibility(&a, &params(), now(), false), None);
    }

    #[test]
    fn auth_unhealthy_gates_first() {
        let mut a = account("a");
        a.healthy = false;
        a.five_hour = Some(window(1.0, HOUR));
        a.cooldown_until = Some(at(NOW_SECS + 60));
        assert_eq!(
            eligibility(&a, &params(), now(), false),
            Some(IneligibleReason::AuthUnhealthy)
        );
    }

    #[test]
    fn cooldown_gates_until_it_expires() {
        let mut a = account("a");
        a.cooldown_until = Some(at(NOW_SECS + 1));
        a.cooldown_source = Some(CooldownSource::RetryAfter);
        assert_eq!(
            eligibility(&a, &params(), now(), false),
            Some(IneligibleReason::CoolingDown)
        );
        // At/after cooldown_until the park is over.
        assert_eq!(eligibility(&a, &params(), at(NOW_SECS + 1), false), None);
    }

    #[test]
    fn stale_usage_gates_unless_headers_only() {
        let mut a = account("a");
        // Fetched 601s ago (max age 600), still-live window.
        a.five_hour = Some(window_fetched(0.10, HOUR, NOW_SECS - 601));
        assert_eq!(
            eligibility(&a, &params(), now(), false),
            Some(IneligibleReason::UsageStale)
        );
        assert_eq!(
            eligibility(&a, &params(), now(), true),
            None,
            "headers-only mode drops the staleness gate"
        );
    }

    #[test]
    fn codex_accounts_are_exempt_from_the_staleness_gate() {
        // Same stale-but-live window shape as
        // `stale_usage_gates_unless_headers_only` — but on a codex account
        // it must NOT gate: codex has no usage poller, headers are its only
        // (and legitimately old) window source.
        let mut codex = account("cx");
        codex.credential_kind = "codex";
        codex.five_hour = Some(window_fetched(0.10, HOUR, NOW_SECS - 601));
        assert_eq!(eligibility(&codex, &params(), now(), false), None);
        // The quota gates themselves still apply to codex.
        codex.seven_day = Some(window_fetched(0.995, 24 * HOUR, NOW_SECS - 601));
        assert_eq!(
            eligibility(&codex, &params(), now(), false),
            Some(IneligibleReason::SevenDayOverThreshold)
        );
    }

    #[test]
    fn stale_but_expired_data_degrades_to_cold_not_stale() {
        let mut a = account("a");
        // Old observation whose window already reset: carries no constraint.
        a.five_hour = Some(QuotaWindow {
            utilization: 1.0,
            resets_at: at(NOW_SECS - HOUR),
            fetched_at: at(NOW_SECS - 2 * HOUR),
            source: WindowSource::Headers,
        });
        assert_eq!(eligibility(&a, &params(), now(), false), None);
    }

    // ---- ranking ----

    #[test]
    fn ranks_by_soonest_seven_day_reset() {
        let mut a = account("a");
        a.seven_day = Some(window(0.5, 48 * HOUR));
        let mut b = account("b");
        b.seven_day = Some(window(0.5, 12 * HOUR)); // resets sooner: use it or lose it
        let decision = pick(&pool(vec![a, b], None), &params(), None, now());
        assert_eq!(decision, Decision::Switch { to: id("b") });
    }

    #[test]
    fn tiebreaks_by_lower_five_hour_utilization() {
        let mut a = account("a");
        a.seven_day = Some(window(0.5, 24 * HOUR));
        a.five_hour = Some(window(0.60, HOUR));
        let mut b = account("b");
        b.seven_day = Some(window(0.5, 24 * HOUR));
        b.five_hour = Some(window(0.20, HOUR));
        let decision = pick(&pool(vec![a, b], None), &params(), None, now());
        assert_eq!(decision, Decision::Switch { to: id("b") });
    }

    #[test]
    fn final_tiebreak_is_stable_id_order() {
        let decision = pick(
            &pool(vec![account("bravo"), account("alpha")], None),
            &params(),
            None,
            now(),
        );
        assert_eq!(decision, Decision::Switch { to: id("alpha") });
    }

    // ---- score-based ranking (5h-rate-aware, .prd/07-scheduler-research.md) ----

    #[test]
    fn account_score_is_servable_now_times_urgency() {
        // Reset 6 days out (frac_left = 6/7) → mild urgency 1 + 3*(1 - 6/7) =
        // 10/7; servable = min(r5 0.70, r7 0.89) = 0.70 → score 0.70 * 10/7 = 1.0.
        let mut comfy = account("c");
        comfy.five_hour = Some(window(0.20, HOUR)); // r5 = 0.70
        comfy.seven_day = Some(window(0.10, 6 * 24 * HOUR)); // r7 = 0.89
        assert!((account_score(&comfy, &params(), now()) - 1.0).abs() < 1e-9);

        // Resets in 1h (frac_left = 1/168) → urgency 1 + 3*(167/168) = 669/168;
        // servable 0.70 → score 0.70 * 669/168 = 2.7875.
        let mut urgent = account("u");
        urgent.five_hour = Some(window(0.20, HOUR)); // r5 = 0.70
        urgent.seven_day = Some(window(0.10, HOUR)); // r7 = 0.89, resets in 1h
        assert!((account_score(&urgent, &params(), now()) - 2.7875).abs() < 1e-9);
    }

    #[test]
    fn perishable_known_reset_outranks_fuller_cold_account() {
        // Use-it-or-lose-it: an account half-used but resetting in ~1 day
        // (frac_left 1/7 → urgency 25/7 ≈ 3.57, score 0.49 * 3.57 ≈ 1.75) beats a
        // full cold account (score 0.90). The cold account has a long runway and
        // is preserved as a reservoir; the perishable one is burned first.
        let cold = account("aaa"); // full budget, no live 7d → score 0.90
        let mut known = account("zzz");
        known.seven_day = Some(window(0.5, 24 * HOUR)); // resets in 1 day → perishable
        let decision = pick(&pool(vec![cold, known], None), &params(), None, now());
        assert_eq!(
            decision,
            Decision::Switch { to: id("zzz") },
            "soon-resetting, still-usable weekly quota is burned before a long-runway account"
        );
    }

    #[test]
    fn urgent_perishable_quota_is_burned_first() {
        // An account whose 7d resets within one 5h window, still has weekly
        // budget, and is usable now → its salvageable slice is perishable, so it
        // beats the fuller-but-safe account (use-it-or-lose-it, but targeted).
        let safe = account("safe"); // cold → score 0.90, urgency 1
        let mut urgent = account("urgent");
        urgent.seven_day = Some(window(0.5, 2 * HOUR)); // resets <1 window → urgency max → 1.47
        let decision = pick(&pool(vec![safe, urgent], None), &params(), None, now());
        assert_eq!(
            decision,
            Decision::Switch { to: id("urgent") },
            "perishable, usable weekly quota is salvaged first"
        );
    }

    #[test]
    fn urgent_but_5h_gated_quota_is_not_chased() {
        // Same imminent 7d reset, but the 5h window is maxed: no usable burst
        // remains, so the urgency boost cannot rescue it (servable_now → 0). A
        // usable account is preferred over stalling on an unusable urgent one.
        let mut urgent = account("urgent");
        urgent.seven_day = Some(window(0.5, 2 * HOUR)); // perishable
        urgent.five_hour = Some(window(0.90, HOUR)); // r5 = 0 → servable_now = 0 → score 0
        let usable = account("usable"); // cold → score 0.90
        let decision = pick(&pool(vec![urgent, usable], None), &params(), None, now());
        assert_eq!(
            decision,
            Decision::Switch { to: id("usable") },
            "an urgent account with no usable burst left does not win"
        );
    }

    #[test]
    fn five_hour_rate_caps_an_account_rich_in_weekly_budget() {
        // a: lots of 7d budget but a nearly-full 5h window → can serve almost
        // nothing now. b: less 7d budget but a fresh 5h window. servable_now =
        // min(r5, r7) prefers the account usable right now.
        let mut a = account("a");
        a.seven_day = Some(window(0.0, 24 * HOUR)); // r7 = 0.99
        a.five_hour = Some(window(0.88, HOUR)); // r5 = 0.02 → servable 0.02
        let mut b = account("b");
        b.seven_day = Some(window(0.7, 24 * HOUR)); // r7 = 0.29
        b.five_hour = Some(window(0.10, HOUR)); // r5 = 0.80 → servable 0.29
        let decision = pick(&pool(vec![a, b], None), &params(), None, now());
        assert_eq!(
            decision,
            Decision::Switch { to: id("b") },
            "the account you can actually burst from now beats a 5h-gated one"
        );
    }

    fn codex_account(id: &str) -> AccountSnapshot {
        let mut a = account(id);
        a.credential_kind = "codex";
        a.group = BackendGroup::Codex;
        a
    }

    /// Snapshot with explicit per-group current slots.
    fn pool_groups(
        accounts: Vec<AccountSnapshot>,
        currents: &[(BackendGroup, &str)],
    ) -> PoolSnapshot {
        let mut map = std::collections::BTreeMap::new();
        for (g, c) in currents {
            map.insert(*g, AccountId(c.to_string()));
        }
        PoolSnapshot {
            accounts,
            current: map,
            fable_current: std::collections::BTreeMap::new(),
            manual_pin: std::collections::BTreeMap::new(),
        }
    }

    // ---- next_in_line (per-group) ----

    #[test]
    fn next_in_line_returns_best_eligible_non_current_in_group() {
        let snap = pool(vec![account("a"), account("b")], Some("a"));
        assert_eq!(
            next_in_line(&snap, &params(), now(), Some(BackendGroup::Claude)),
            Some(id("b"))
        );
    }

    #[test]
    fn next_in_line_is_none_when_group_has_only_its_current() {
        let snap = pool_groups(vec![codex_account("c")], &[(BackendGroup::Codex, "c")]);
        assert_eq!(
            next_in_line(&snap, &params(), now(), Some(BackendGroup::Codex)),
            None
        );
    }

    #[test]
    fn next_in_line_is_scoped_to_its_own_group() {
        // claude a(current)+b, codex c(current). Claude's next is b (never the
        // codex account); codex has no other account so its next is none.
        let snap = pool_groups(
            vec![account("a"), account("b"), codex_account("c")],
            &[(BackendGroup::Claude, "a"), (BackendGroup::Codex, "c")],
        );
        assert_eq!(
            next_in_line(&snap, &params(), now(), Some(BackendGroup::Claude)),
            Some(id("b"))
        );
        assert_eq!(
            next_in_line(&snap, &params(), now(), Some(BackendGroup::Codex)),
            None
        );
    }

    #[test]
    fn codex_ranks_after_cold_anthropic_accounts() {
        // "aaa" would beat "zzz" on the id tiebreak — the codex tier must
        // override that: codex is the overflow pool, never the default pick.
        let codex = codex_account("aaa");
        let anthropic = account("zzz");
        let decision = pick(&pool(vec![codex, anthropic], None), &params(), None, now());
        assert_eq!(decision, Decision::Switch { to: id("zzz") });
    }

    #[test]
    fn codex_ranks_after_anthropic_with_known_resets() {
        let codex = codex_account("a-codex");
        let mut anthropic = account("b-known");
        anthropic.seven_day = Some(window(0.5, 48 * HOUR));
        let decision = pick(&pool(vec![codex, anthropic], None), &params(), None, now());
        assert_eq!(decision, Decision::Switch { to: id("b-known") });
    }

    #[test]
    fn codex_is_picked_when_every_anthropic_account_is_ineligible() {
        let codex = codex_account("codex");
        let mut over = account("anthropic");
        over.five_hour = Some(window(0.95, HOUR));
        let decision = pick(&pool(vec![codex, over], None), &params(), None, now());
        assert_eq!(
            decision,
            Decision::Switch { to: id("codex") },
            "overflow pool serves when anthropic quota is gone"
        );
    }

    #[test]
    fn codex_never_gates_on_staleness_or_windows() {
        let codex = codex_account("codex");
        assert_eq!(
            eligibility(&codex, &params(), now(), false),
            None,
            "no windows, no usage polling — always eligible while healthy"
        );
    }

    #[test]
    fn selection_order_puts_codex_after_eligible_anthropic() {
        let codex = codex_account("a-codex");
        let anthropic = account("b-anthropic");
        let snapshot = pool(vec![codex, anthropic], None);
        assert_eq!(ordered_ids(&snapshot), vec!["b-anthropic", "a-codex"]);
    }

    // ---- group filter (routing enabled) ----

    /// Build a snapshot whose per-group current slots are set explicitly.
    fn pool_with_groups(
        accounts: Vec<AccountSnapshot>,
        slots: &[(BackendGroup, &str)],
    ) -> PoolSnapshot {
        let mut current = std::collections::BTreeMap::new();
        for (g, c) in slots {
            current.insert(*g, AccountId(c.to_string()));
        }
        PoolSnapshot {
            accounts,
            current,
            fable_current: std::collections::BTreeMap::new(),
            manual_pin: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn group_filter_picks_only_in_group_account() {
        // "aaa" (codex) would win id order, but the claude group must pick the
        // claude account; the codex group must pick the codex account.
        let codex = codex_account("aaa");
        let claude = account("zzz");
        let snapshot = pool(vec![codex, claude], None);
        assert_eq!(
            pick(&snapshot, &params(), Some(BackendGroup::Claude), now()),
            Decision::Switch { to: id("zzz") },
            "claude group ignores the codex account"
        );
        assert_eq!(
            pick(&snapshot, &params(), Some(BackendGroup::Codex), now()),
            Decision::Switch { to: id("aaa") },
            "codex group ignores the claude account"
        );
    }

    #[test]
    fn group_filter_codex_tier_rule_is_a_no_op_under_filter() {
        // Two codex accounts: under the codex group filter the tier rule is a
        // no-op, so ranking falls through to id order ("a" before "z").
        let a = codex_account("a");
        let z = codex_account("z");
        let snapshot = pool(vec![z, a], None);
        assert_eq!(
            pick(&snapshot, &params(), Some(BackendGroup::Codex), now()),
            Decision::Switch { to: id("a") }
        );
    }

    #[test]
    fn group_stickiness_is_independent_per_group() {
        // Claude slot points at the claude account, codex slot at the codex
        // account: each group stays on its own sticky pick even though a
        // higher-ranked account exists in the other group.
        let claude = account("claude-cur");
        let claude_alt = account("claude-alt");
        let codex = codex_account("codex-cur");
        let snapshot = pool_with_groups(
            vec![claude, claude_alt, codex],
            &[
                (BackendGroup::Claude, "claude-cur"),
                (BackendGroup::Codex, "codex-cur"),
            ],
        );
        assert_eq!(
            pick(&snapshot, &params(), Some(BackendGroup::Claude), now()),
            Decision::Stay,
            "claude group stays on its sticky current"
        );
        assert_eq!(
            pick(&snapshot, &params(), Some(BackendGroup::Codex), now()),
            Decision::Stay,
            "codex group stays on its sticky current, independent of claude"
        );
    }

    #[test]
    fn empty_group_is_exhausted() {
        // Only a claude account: the codex group has nothing to select.
        let snapshot = pool(vec![account("a")], None);
        assert_eq!(
            pick(&snapshot, &params(), Some(BackendGroup::Codex), now()),
            Decision::Exhausted { retry_after: None },
            "empty group exhausts without picking the other group"
        );
        // The claude group still selects fine.
        assert_eq!(
            pick(&snapshot, &params(), Some(BackendGroup::Claude), now()),
            Decision::Switch { to: id("a") }
        );
    }

    // ---- stickiness ----

    #[test]
    fn proactively_switches_off_eligible_current_for_clearly_more_perishable() {
        // The current account is eligible but low-value (5h 0.80 → servable 0.10,
        // reset 2 days out → score ≈ 0.31). Another account resets within the hour
        // with a fresh 5h window (servable 0.49 → score ≈ 1.95) — over 6× the
        // current, well past SWITCH_MARGIN, so the scheduler moves to burn it.
        let mut current = account("a");
        current.seven_day = Some(window(0.5, 48 * HOUR));
        current.five_hour = Some(window(0.80, HOUR));
        let mut better = account("b");
        better.seven_day = Some(window(0.5, HOUR));
        better.five_hour = Some(window(0.10, HOUR));
        let decision = pick(
            &pool(vec![current, better], Some("a")),
            &params(),
            None,
            now(),
        );
        assert_eq!(decision, Decision::Switch { to: id("b") });
    }

    #[test]
    fn stays_on_eligible_current_within_switch_margin() {
        // The alternative outranks the current, but only by ~6% (0.85 vs 0.80),
        // under SWITCH_MARGIN (25%). Not worth a prompt-cache-busting hand-off —
        // stickiness holds.
        let mut current = account("a");
        current.five_hour = Some(window(0.10, HOUR)); // servable 0.80, urgency 1
        let mut alt = account("b");
        alt.five_hour = Some(window(0.05, HOUR)); // servable 0.85, urgency 1
        let decision = pick(&pool(vec![current, alt], Some("a")), &params(), None, now());
        assert_eq!(decision, Decision::Stay);
    }

    #[test]
    fn switches_when_current_crosses_threshold() {
        let mut current = account("a");
        current.five_hour = Some(window(0.95, HOUR)); // over 0.90
        let fallback = account("b");
        let decision = pick(
            &pool(vec![current, fallback], Some("a")),
            &params(),
            None,
            now(),
        );
        assert_eq!(decision, Decision::Switch { to: id("b") });
    }

    #[test]
    fn switches_when_current_is_cooling_down() {
        let mut current = account("a");
        current.cooldown_until = Some(at(NOW_SECS + 120));
        let fallback = account("b");
        let decision = pick(
            &pool(vec![current, fallback], Some("a")),
            &params(),
            None,
            now(),
        );
        assert_eq!(decision, Decision::Switch { to: id("b") });
    }

    #[test]
    fn switches_when_current_auth_fails() {
        let mut current = account("a");
        current.healthy = false;
        let fallback = account("b");
        let decision = pick(
            &pool(vec![current, fallback], Some("a")),
            &params(),
            None,
            now(),
        );
        assert_eq!(decision, Decision::Switch { to: id("b") });
    }

    #[test]
    fn initial_selection_with_no_current_switches() {
        let decision = pick(&pool(vec![account("a")], None), &params(), None, now());
        assert_eq!(decision, Decision::Switch { to: id("a") });
    }

    // ---- all-stale degraded mode ----

    #[test]
    fn all_stale_enables_headers_only_mode() {
        let mut a = account("a");
        a.five_hour = Some(window_fetched(0.10, HOUR, NOW_SECS - 5000));
        let mut b = account("b");
        b.five_hour = Some(window_fetched(0.50, HOUR, NOW_SECS - 5000));
        let snapshot = pool(vec![a, b], None);
        assert!(headers_only_mode(&snapshot, &params(), None, now()));
        // Staleness gate dropped: scheduling proceeds on the stale data.
        assert_eq!(
            pick(&snapshot, &params(), None, now()),
            Decision::Switch { to: id("a") }
        );
    }

    #[test]
    fn stale_account_loses_to_fresh_cold_account() {
        let mut stale = account("a");
        stale.five_hour = Some(window_fetched(0.10, HOUR, NOW_SECS - 5000));
        let cold = account("b");
        let snapshot = pool(vec![stale, cold], None);
        assert!(!headers_only_mode(&snapshot, &params(), None, now()));
        assert_eq!(
            pick(&snapshot, &params(), None, now()),
            Decision::Switch { to: id("b") }
        );
    }

    #[test]
    fn staleness_gate_drops_when_it_is_the_only_blocker() {
        // A is genuinely over threshold, B is merely stale: serving B beats
        // refusing service (upstream 429 is the corrective backstop).
        let mut over = account("a");
        over.five_hour = Some(window(0.95, HOUR));
        let mut stale = account("b");
        stale.five_hour = Some(window_fetched(0.10, HOUR, NOW_SECS - 5000));
        let snapshot = pool(vec![over, stale], None);
        assert!(headers_only_mode(&snapshot, &params(), None, now()));
        assert_eq!(
            pick(&snapshot, &params(), None, now()),
            Decision::Switch { to: id("b") }
        );
    }

    #[test]
    fn sticky_current_survives_in_headers_only_mode() {
        let mut a = account("a");
        a.five_hour = Some(window_fetched(0.10, HOUR, NOW_SECS - 5000));
        let mut b = account("b");
        b.five_hour = Some(window_fetched(0.05, HOUR, NOW_SECS - 5000));
        let decision = pick(&pool(vec![a, b], Some("a")), &params(), None, now());
        assert_eq!(decision, Decision::Stay);
    }

    // ---- heuristic-degraded mode (retry-after-less 429 lockout recovery) ----

    /// Account parked by a cooldown of `source`, freeing `free_in` secs out.
    fn parked(id_: &str, source: CooldownSource, free_in: u64) -> AccountSnapshot {
        let mut a = account(id_);
        a.cooldown_until = Some(at(NOW_SECS + free_in));
        a.cooldown_source = Some(source);
        a
    }

    #[test]
    fn all_heuristic_parked_enables_degraded_mode_and_picks_soonest_freed() {
        // Whole pool parked by retry-after-less 429s (Heuristic). No account
        // passes the full gate, so degraded mode activates and the SOONEST-freed
        // account is chosen — not a hard exhaust.
        let a = parked("a", CooldownSource::Heuristic, 8);
        let b = parked("b", CooldownSource::Heuristic, 3); // frees soonest
        let snapshot = pool(vec![a, b], None);
        assert!(heuristic_degraded_mode(&snapshot, &params(), None, now()));
        assert_eq!(
            pick(&snapshot, &params(), None, now()),
            Decision::Switch { to: id("b") },
            "degraded mode serves the soonest-freed heuristic-parked account"
        );
    }

    #[test]
    fn one_eligible_account_disables_degraded_mode() {
        // A is heuristic-parked but B is fresh and usable: no lockout, so
        // degraded mode stays OFF and the normal selector picks the live B.
        let a = parked("a", CooldownSource::Heuristic, 8);
        let b = account("b");
        let snapshot = pool(vec![a, b], None);
        assert!(!heuristic_degraded_mode(&snapshot, &params(), None, now()));
        assert_eq!(
            pick(&snapshot, &params(), None, now()),
            Decision::Switch { to: id("b") }
        );
    }

    #[test]
    fn retry_after_park_is_not_bypassed_by_degraded_mode() {
        // A RetryAfter park is a real quota signal: even when it is the only
        // blocker, degraded mode must NOT activate and the pool exhausts.
        let a = parked("a", CooldownSource::RetryAfter, 120);
        let b = parked("b", CooldownSource::RetryAfter, 120);
        let snapshot = pool(vec![a, b], None);
        assert!(!heuristic_degraded_mode(&snapshot, &params(), None, now()));
        assert!(
            matches!(
                pick(&snapshot, &params(), None, now()),
                Decision::Exhausted { .. }
            ),
            "retry-after parks stay gated — no degraded bypass"
        );
    }

    #[test]
    fn auth_unhealthy_is_not_bypassed_by_degraded_mode() {
        // One account heuristic-parked, the other auth-failed. The auth-failed
        // one must STAY gated; only the heuristic-parked one can be degraded to.
        let a = parked("a", CooldownSource::Heuristic, 5);
        let mut dead = account("b");
        dead.healthy = false;
        let snapshot = pool(vec![a, dead], None);
        assert!(heuristic_degraded_mode(&snapshot, &params(), None, now()));
        assert_eq!(
            pick(&snapshot, &params(), None, now()),
            Decision::Switch { to: id("a") },
            "degrades onto the heuristic-parked account, never the auth-failed one"
        );
    }

    #[test]
    fn five_hour_over_threshold_is_not_bypassed_by_degraded_mode() {
        // A heuristic-parked account that is ALSO over its 5h quota would still
        // gate on the quota after the cooldown is dropped — so it does not make
        // the pool degradable. With it the only blocker besides nothing, the
        // pool exhausts (real quota, not a transient blip).
        let mut over = parked("a", CooldownSource::Heuristic, 5);
        over.five_hour = Some(window(0.95, HOUR)); // over 0.90 even if unparked
        let snapshot = pool(vec![over], None);
        assert!(
            !heuristic_degraded_mode(&snapshot, &params(), None, now()),
            "a real-quota-over account is not merely heuristic-blocked"
        );
        assert!(matches!(
            pick(&snapshot, &params(), None, now()),
            Decision::Exhausted { .. }
        ));
    }

    #[test]
    fn degraded_mode_prefers_heuristic_freed_over_a_separate_retry_after_park() {
        // a: heuristic-parked, frees in 8s. b: RetryAfter-parked, frees in 2s.
        // The RetryAfter park is NOT bypassable, so degraded mode picks a (the
        // soonest-freed account AMONG the heuristic-eligible ones), not b.
        let a = parked("a", CooldownSource::Heuristic, 8);
        let b = parked("b", CooldownSource::RetryAfter, 2);
        let snapshot = pool(vec![a, b], None);
        assert!(heuristic_degraded_mode(&snapshot, &params(), None, now()));
        assert_eq!(
            pick(&snapshot, &params(), None, now()),
            Decision::Switch { to: id("a") }
        );
    }

    #[test]
    fn degraded_mode_is_scoped_per_group() {
        // Claude group fully heuristic-parked; a usable codex account exists.
        // The claude group degrades onto its own parked account — it must not
        // be flipped off degraded mode by the codex account, nor pick it.
        let claude = parked("claude", CooldownSource::Heuristic, 5);
        let codex = codex_account("codex");
        let snapshot = pool_groups(vec![claude, codex], &[]);
        assert!(heuristic_degraded_mode(
            &snapshot,
            &params(),
            Some(BackendGroup::Claude),
            now()
        ));
        assert_eq!(
            pick(&snapshot, &params(), Some(BackendGroup::Claude), now()),
            Decision::Switch { to: id("claude") }
        );
        // The codex group is not in degraded mode (its account is usable).
        assert!(!heuristic_degraded_mode(
            &snapshot,
            &params(),
            Some(BackendGroup::Codex),
            now()
        ));
    }

    // ---- exhaustion + soonest reset ----

    #[test]
    fn all_exhausted_reports_soonest_reset() {
        let mut a = account("a");
        a.five_hour = Some(window(0.95, 3 * HOUR));
        a.seven_day = Some(window(0.5, 48 * HOUR));
        let mut b = account("b");
        b.five_hour = Some(window(0.99, 2 * HOUR)); // soonest future reset
        b.seven_day = Some(window(0.5, 24 * HOUR));
        let decision = pick(&pool(vec![a, b], None), &params(), None, now());
        assert_eq!(
            decision,
            Decision::Exhausted {
                retry_after: Some(Duration::from_secs(2 * HOUR)),
            }
        );
    }

    #[test]
    fn short_cooldown_park_wins_soonest_reset() {
        let mut a = account("a");
        a.five_hour = Some(window(0.95, 3 * HOUR));
        a.cooldown_until = Some(at(NOW_SECS + 2)); // 429 retry-after: 2
        let snapshot = pool(vec![a], None);
        assert_eq!(
            pick(&snapshot, &params(), None, now()),
            Decision::Exhausted {
                retry_after: Some(Duration::from_secs(2)),
            }
        );
    }

    #[test]
    fn unhealthy_accounts_do_not_contribute_resets() {
        let mut dead = account("a");
        dead.healthy = false;
        dead.five_hour = Some(window(1.0, 60)); // would be the soonest
        let mut parked = account("b");
        parked.cooldown_until = Some(at(NOW_SECS + 300));
        let snapshot = pool(vec![dead, parked], None);
        assert_eq!(
            soonest_reset(&snapshot, now()),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn empty_pool_is_exhausted_with_no_retry_hint() {
        assert_eq!(
            pick(&pool(vec![], None), &params(), None, now()),
            Decision::Exhausted { retry_after: None }
        );
    }

    // ---- selection order (B1) ----

    /// Pull the display-order ids out for assertion readability.
    fn ordered_ids(snapshot: &PoolSnapshot) -> Vec<String> {
        selection_order(snapshot, &params(), now())
            .into_iter()
            .map(|i| snapshot.accounts[i].id.0.clone())
            .collect()
    }

    #[test]
    fn order_is_current_then_rank_then_ineligible() {
        let mut current = account("cur");
        current.seven_day = Some(window(0.5, 72 * HOUR)); // worst rank of the eligibles
        let mut soon = account("soon");
        soon.seven_day = Some(window(0.5, 12 * HOUR)); // best rank
        let mut later = account("later");
        later.seven_day = Some(window(0.5, 48 * HOUR));
        let mut parked = account("parked");
        parked.cooldown_until = Some(at(NOW_SECS + 60));
        let mut dead = account("dead");
        dead.healthy = false;
        let snapshot = pool(vec![parked, later, soon, dead, current], Some("cur"));
        assert_eq!(
            ordered_ids(&snapshot),
            vec!["cur", "soon", "later", "parked", "dead"],
            "current first, eligibles by rank, ineligibles last in stable order"
        );
    }

    #[test]
    fn order_keeps_ineligible_current_first() {
        // The current account just crossed a threshold: the scheduler will
        // move off it on the next evaluation, but it IS still current.
        let mut current = account("cur");
        current.five_hour = Some(window(0.95, HOUR));
        let other = account("other");
        let snapshot = pool(vec![other, current], Some("cur"));
        assert_eq!(ordered_ids(&snapshot), vec!["cur", "other"]);
    }

    #[test]
    fn order_matches_pick_for_the_next_account() {
        let mut a = account("a");
        a.seven_day = Some(window(0.5, 48 * HOUR));
        let mut b = account("b");
        b.seven_day = Some(window(0.5, 12 * HOUR));
        let snapshot = pool(vec![a, b], None);
        let order = selection_order(&snapshot, &params(), now());
        let first = &snapshot.accounts[order[0]].id;
        assert_eq!(
            pick(&snapshot, &params(), None, now()),
            Decision::Switch { to: first.clone() },
            "head of the order is exactly what pick would choose"
        );
    }

    #[test]
    fn order_covers_every_account_exactly_once() {
        let snapshot = pool(vec![account("a"), account("b"), account("c")], Some("b"));
        let mut order = selection_order(&snapshot, &params(), now());
        order.sort_unstable();
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn order_uses_headers_only_mode_like_pick() {
        // All accounts stale → staleness gate drops; both stay "eligible"
        // and are ranked instead of dumped into the ineligible tail.
        let mut a = account("a");
        a.five_hour = Some(window_fetched(0.50, HOUR, NOW_SECS - 5000));
        a.seven_day = Some(window_fetched(0.5, 48 * HOUR, NOW_SECS - 5000));
        let mut b = account("b");
        b.five_hour = Some(window_fetched(0.10, HOUR, NOW_SECS - 5000));
        b.seven_day = Some(window_fetched(0.5, 12 * HOUR, NOW_SECS - 5000));
        let snapshot = pool(vec![a, b], None);
        assert_eq!(ordered_ids(&snapshot), vec!["b", "a"], "ranked, not parked");
    }

    // ---- blocking reasons ----

    #[test]
    fn blocking_reason_strings_carry_the_numbers() {
        let mut parked = account("a");
        parked.cooldown_until = Some(at(NOW_SECS + 192));
        assert_eq!(
            blocking_reason(&parked, IneligibleReason::CoolingDown, &params(), now()),
            "cooldown 3m12s"
        );

        let mut over5 = account("b");
        over5.five_hour = Some(window(0.95, HOUR));
        assert_eq!(
            blocking_reason(
                &over5,
                IneligibleReason::FiveHourOverThreshold,
                &params(),
                now()
            ),
            "5h 95.0% > 90%"
        );

        let mut over7 = account("c");
        over7.seven_day = Some(window(0.994, 24 * HOUR));
        assert_eq!(
            blocking_reason(
                &over7,
                IneligibleReason::SevenDayOverThreshold,
                &params(),
                now()
            ),
            "7d 99.4% > 99%"
        );

        let mut stale = account("d");
        stale.five_hour = Some(window_fetched(0.10, HOUR, NOW_SECS - 14 * 60));
        assert_eq!(
            blocking_reason(&stale, IneligibleReason::UsageStale, &params(), now()),
            "usage stale 14m00s"
        );

        let mut dead = account("e");
        dead.healthy = false;
        assert_eq!(
            blocking_reason(&dead, IneligibleReason::AuthUnhealthy, &params(), now()),
            "auth failed"
        );
    }

    #[test]
    fn blocking_reason_degrades_without_timestamps() {
        let parked = account("a"); // CoolingDown claimed but no cooldown_until
        assert_eq!(
            blocking_reason(&parked, IneligibleReason::CoolingDown, &params(), now()),
            "cooldown"
        );
        let stale = account("b"); // no live observation to age
        assert_eq!(
            blocking_reason(&stale, IneligibleReason::UsageStale, &params(), now()),
            "usage stale"
        );
    }

    #[test]
    fn compact_duration_bands() {
        assert_eq!(compact_duration(Duration::from_secs(42)), "42s");
        assert_eq!(compact_duration(Duration::from_secs(192)), "3m12s");
        assert_eq!(
            compact_duration(Duration::from_secs(6 * 3600 + 52 * 60)),
            "6h52m"
        );
        assert_eq!(
            compact_duration(Duration::from_secs(2 * 86_400 + 4 * 3600)),
            "2d4h"
        );
        assert_eq!(compact_duration(Duration::ZERO), "0s");
    }

    #[test]
    fn past_resets_are_ignored_by_soonest_reset() {
        let mut a = account("a");
        a.five_hour = Some(QuotaWindow {
            utilization: 1.0,
            resets_at: at(NOW_SECS - 100),
            fetched_at: at(NOW_SECS - 200),
            source: WindowSource::Headers,
        });
        a.seven_day = Some(window(0.5, 500));
        assert_eq!(
            soonest_reset(&pool(vec![a], None), now()),
            Some(Duration::from_secs(500))
        );
    }

    // ---- perishability policy verification (.prd/09-scheduler-perishability.md) ----

    /// Account with both windows live: 5h `u5`, 7d `u7`, 7d resets `reset_h`
    /// hours out. The 5h window's own reset is parked far enough out to never
    /// expire during a test.
    fn acct7(id_: &str, u5: f64, u7: f64, reset_h: u64) -> AccountSnapshot {
        let mut a = account(id_);
        a.five_hour = Some(window(u5, 12 * HOUR));
        a.seven_day = Some(window(u7, reset_h * HOUR));
        a
    }

    /// The live fleet from the bug report: six idle Claude accounts, all far
    /// under their limits, 7d resets spread 1.3d–6.4d. The OLD scheduler picked
    /// dev1 (most 5h headroom) and camped ai2 — both with the LONGEST runway —
    /// while ai3/notify (barely-used quota, resets ~1 day) sat unused. The fix
    /// must pick the soonest-resetting *usable* account and proactively leave
    /// the long-runway current.
    fn owner_fleet() -> Vec<AccountSnapshot> {
        vec![
            acct7("ai2", 0.17, 0.04, 101),   // 4d5h
            acct7("dev1", 0.03, 0.02, 136),  // 5d16h
            acct7("info", 0.04, 0.04, 147),  // 6d3h
            acct7("notify", 0.05, 0.03, 38), // 1d14h
            acct7("ai3", 0.06, 0.06, 30),    // 1d6h  (soonest)
            acct7("ai", 0.17, 0.04, 154),    // 6d10h
        ]
    }

    #[test]
    fn owner_fleet_burns_soonest_reset_first_not_most_headroom() {
        let snap = pool(owner_fleet(), Some("ai2"));
        // Proactively leaves the eligible-but-long-runway current (ai2) for the
        // most-perishable usable account (ai3, resets in ~1d6h).
        assert_eq!(
            pick(&snap, &params(), None, now()),
            Decision::Switch { to: id("ai3") },
            "must switch off ai2 to ai3 (soonest reset), NOT stay or pick dev1"
        );
        // The displayed "next" agrees with the selector.
        assert_eq!(next_in_line(&snap, &params(), now(), None), Some(id("ai3")));
        // Full ranked order: current first, then perishable-first.
        let order: Vec<String> = selection_order(&snap, &params(), now())
            .into_iter()
            .map(|i| snap.accounts[i].id.0.clone())
            .collect();
        assert_eq!(
            order,
            vec!["ai2", "ai3", "notify", "dev1", "info", "ai"],
            "ai3 and notify (resets ~1 day) outrank the 4–6 day accounts"
        );
    }

    #[test]
    fn owner_fleet_burn_down_sequence_visits_accounts_in_perishability_order() {
        // Drain the fleet: each round pick the best, then max its 5h window so it
        // can no longer serve (servable → 0), and pick again. The SEQUENCE of
        // first picks must be soonest-reset-among-usable throughout — never a
        // long-runway account while a more-perishable usable one remains.
        let mut accts = owner_fleet();
        let mut sequence = Vec::new();
        for _ in 0..accts.len() {
            let snap = pool(accts.clone(), None);
            let Decision::Switch { to } = pick(&snap, &params(), None, now()) else {
                panic!("expected a switch while usable accounts remain");
            };
            sequence.push(to.0.clone());
            let idx = accts
                .iter()
                .position(|a| a.id == to)
                .expect("picked account");
            // Gate its 5h window (still eligible at the boundary, but servable 0).
            accts[idx].five_hour = Some(window(0.90, 12 * HOUR));
        }
        assert_eq!(
            sequence,
            vec!["ai3", "notify", "ai2", "dev1", "info", "ai"],
            "perishable-first burn-down: 1-day accounts, then by soonest reset"
        );
    }

    // ---- comparator invariants ----

    /// A deliberately diverse fleet: cold, mixed util/reset, 5h-only, 7d-only,
    /// fully-gated, and a codex overflow account — to stress the comparator.
    fn diverse_fleet() -> Vec<AccountSnapshot> {
        let mut five_only = account("five_only");
        five_only.five_hour = Some(window(0.30, 12 * HOUR));
        let mut seven_only = account("seven_only");
        seven_only.seven_day = Some(window(0.40, 50 * HOUR));
        vec![
            account("cold"),
            acct7("soon_light", 0.10, 0.10, 6),
            acct7("mid", 0.40, 0.50, 60),
            acct7("far_light", 0.05, 0.02, 150),
            acct7("gated5", 0.90, 0.10, 20),
            acct7("full7", 0.10, 1.00, 20),
            five_only,
            seven_only,
            codex_account("cx"),
        ]
    }

    #[test]
    fn rank_is_a_total_order() {
        let fleet = diverse_fleet();
        let le = |a: &AccountSnapshot, b: &AccountSnapshot| {
            rank(a, b, &params(), None, now()) != std::cmp::Ordering::Greater
        };
        // Antisymmetry: a≤b and b≤a ⇒ they compare Equal both ways.
        for a in &fleet {
            for b in &fleet {
                let ab = rank(a, b, &params(), None, now());
                let ba = rank(b, a, &params(), None, now());
                match ab {
                    std::cmp::Ordering::Less => assert_eq!(ba, std::cmp::Ordering::Greater),
                    std::cmp::Ordering::Greater => assert_eq!(ba, std::cmp::Ordering::Less),
                    std::cmp::Ordering::Equal => assert_eq!(ba, std::cmp::Ordering::Equal),
                }
            }
        }
        // Transitivity: a≤b ∧ b≤c ⇒ a≤c, over every triple.
        for a in &fleet {
            for b in &fleet {
                for c in &fleet {
                    if le(a, b) && le(b, c) {
                        assert!(le(a, c), "transitivity violated");
                    }
                }
            }
        }
    }

    #[test]
    fn a_gated_account_never_outranks_a_usable_one() {
        // gated resets very soon (max perishability) but its 5h window is maxed,
        // so servable → 0: it must still rank AFTER a usable, far-reset account.
        let usable = acct7("usable", 0.10, 0.10, 120);
        let gated = acct7("gated", 0.90, 0.10, 1);
        assert!(account_score(&gated, &params(), now()) <= f64::EPSILON);
        assert_eq!(
            rank(&usable, &gated, &params(), None, now()),
            std::cmp::Ordering::Less,
            "usable beats an urgent-but-unusable account"
        );
    }

    #[test]
    fn more_perishable_outranks_equally_usable_less_perishable() {
        // Identical except 7d reset time: the sooner-resetting account scores
        // higher (more urgency, same servable) and ranks first.
        let sooner = acct7("sooner", 0.20, 0.20, 24);
        let later = acct7("later", 0.20, 0.20, 120);
        assert!(account_score(&sooner, &params(), now()) > account_score(&later, &params(), now()));
        assert_eq!(
            rank(&sooner, &later, &params(), None, now()),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn pick_next_and_order_agree_with_no_current() {
        // With no current slot, pick's target, next_in_line, and the head of
        // selection_order must all be the same account.
        let snap = pool(diverse_fleet(), None);
        let Decision::Switch { to } = pick(&snap, &params(), None, now()) else {
            panic!("expected an initial switch");
        };
        assert_eq!(
            next_in_line(&snap, &params(), now(), None),
            Some(to.clone())
        );
        let head = selection_order(&snap, &params(), now())[0];
        assert_eq!(snap.accounts[head].id, to);
    }

    // ---- wasted-quota simulation (the core claim, measured) ----

    /// Phase = one 5h window. Score with the SAME shape production uses, for the
    /// new (linear-over-7d) vs the old (15h-cliff) urgency. `seven_max = 1.0`
    /// keeps weekly budgets clean (0.5 steps); see `sim_score_matches_production`.
    const PHASE_SECS: f64 = 5.0 * 3600.0;
    const SEVEN_SECS: f64 = 7.0 * 24.0 * 3600.0;

    fn sim_score(new_policy: bool, u7: f64, reset_secs: f64) -> f64 {
        let r5 = 0.90_f64; // 5h fresh in this low-utilization regime
        let r7 = (1.0 - u7).max(0.0);
        let servable = r5.min(r7);
        let urgency = if new_policy {
            let frac_left = (reset_secs / SEVEN_SECS).clamp(0.0, 1.0);
            1.0 + (URGENCY_MAX - 1.0) * (1.0 - frac_left)
        } else {
            // The replaced policy: urgency = clamp(3 / windows_left, 1, 3),
            // dormant until the reset is within 3 five-hour windows (15h).
            let windows_left = (reset_secs / PHASE_SECS).max(1.0);
            (3.0 / windows_left).clamp(1.0, 3.0)
        };
        servable * urgency
    }

    #[test]
    fn sim_score_matches_production() {
        // The simulation's new-policy scorer must equal the real account_score
        // (with five_max 0.90, seven_max 1.0, a fresh 5h window).
        let p = SelectParams {
            five_hour_max: 0.90,
            seven_day_max: 1.0,
            fable_weekly_max: 0.98,
            mode: crate::config::SchedulerMode::Default,
            usage_max_age: Duration::from_secs(600),
        };
        for &(u7, reset_h) in &[(0.0, 30u64), (0.5, 10), (0.9, 1), (0.2, 150)] {
            let a = {
                let mut a = account("x");
                a.five_hour = Some(window(0.0, 100 * HOUR));
                a.seven_day = Some(window(u7, reset_h * HOUR));
                a
            };
            let real = account_score(&a, &p, now());
            let sim = sim_score(true, u7, (reset_h as f64) * 3600.0);
            assert!((real - sim).abs() < 1e-9, "u7={u7} reset_h={reset_h}");
        }
    }

    /// Greedy single-consumer simulation: each phase route demand to the
    /// argmax-score account (isolating the RANKING effect — stickiness is tested
    /// separately), serve up to the 5h RATE and the account's weekly budget,
    /// drop the rest. Returns total dropped demand.
    fn simulate(new_policy: bool, demand: &[f64], resets: &[Vec<usize>]) -> f64 {
        const RATE: f64 = 0.5;
        let n = resets.len();
        let mut u7 = vec![0.0_f64; n];
        let mut dropped = 0.0;
        for (p, &d) in demand.iter().enumerate() {
            for (i, sched) in resets.iter().enumerate() {
                if sched.contains(&p) {
                    u7[i] = 0.0; // weekly window reset (any unused budget is lost)
                }
            }
            let mut best = 0usize;
            let mut best_score = f64::NEG_INFINITY;
            for (i, sched) in resets.iter().enumerate() {
                let next = sched
                    .iter()
                    .find(|&&ph| ph > p)
                    .map(|&ph| ph - p)
                    .unwrap_or(100_000);
                let reset_secs = next as f64 * PHASE_SECS;
                let s = sim_score(new_policy, u7[i], reset_secs);
                if s > best_score {
                    best_score = s;
                    best = i;
                }
            }
            let remaining = (1.0 - u7[best]).max(0.0);
            let served = d.min(RATE).min(remaining);
            u7[best] += served;
            dropped += d - served;
        }
        dropped
    }

    #[test]
    fn perishability_policy_wastes_less_than_headroom_policy() {
        // A resets at phase 3 (then never again in-horizon); B never resets.
        // Low early demand, then a burst. The OLD policy load-balances onto B
        // during the lull, leaving A's first-window budget half-unused — lost at
        // A's phase-3 reset — and is then short for the burst. The NEW policy
        // concentrates the lull on the soon-resetting A (fully harvesting it) and
        // preserves B's reservoir, serving the whole burst.
        let demand = [0.5, 0.5, 0.0, 0.5, 0.5, 0.5, 0.5];
        let resets = vec![vec![3usize], vec![]];
        let new_dropped = simulate(true, &demand, &resets);
        let old_dropped = simulate(false, &demand, &resets);
        assert!(
            new_dropped < 1e-9,
            "perishability policy serves all demand (dropped {new_dropped})"
        );
        assert!(
            old_dropped > new_dropped + 0.4,
            "headroom policy wastes a soon-resetting account's budget: \
             old dropped {old_dropped} vs new {new_dropped}"
        );
    }

    // ---- scope-aware gating: Fable cooldown ≠ whole-account cooldown (W2) ----

    /// An account holding a live Fable-scoped cooldown freeing `free_in` secs
    /// out (account-wide state otherwise clean).
    fn fable_cooling(id_: &str, free_in: u64) -> AccountSnapshot {
        let mut a = account(id_);
        a.scoped_cooldowns = vec![ModelScopedCooldown {
            scope: CooldownScope::ModelScoped("Fable".into()),
            until: at(NOW_SECS + free_in),
            set_at: at(NOW_SECS),
            reason: Cooldown429Reason::FableSuspectSnapshotMismatch,
        }];
        a
    }

    /// An account whose Fable weekly bucket reads critical/active (preemptive
    /// exclusion territory), account-wide state otherwise clean.
    fn fable_critical(id_: &str) -> AccountSnapshot {
        let mut a = account(id_);
        a.scoped_limits = vec![ScopedQuotaWindow {
            scope_label: "Fable".into(),
            window: window(1.0, 24 * HOUR),
            severity: LimitSeverity::Critical,
            is_active: true,
        }];
        a
    }

    #[test]
    fn gate_scoped_benches_fable_but_not_non_fable_on_a_fable_cooldown() {
        let a = fable_cooling("a", 8);
        assert_eq!(
            gate_scoped(&a, &params(), now(), false, false, RequestScope::NonFable),
            None,
            "non-Fable request ignores the Fable-scoped cooldown"
        );
        assert_eq!(
            gate_scoped(&a, &params(), now(), false, false, RequestScope::Fable),
            Some(IneligibleReason::FableCoolingDown)
        );
    }

    #[test]
    fn gate_scoped_preemptively_excludes_fable_critical_from_fable_only() {
        let a = fable_critical("a");
        assert_eq!(
            gate_scoped(&a, &params(), now(), false, false, RequestScope::NonFable),
            None,
            "a Fable-critical account still serves non-Fable traffic"
        );
        assert_eq!(
            gate_scoped(&a, &params(), now(), false, false, RequestScope::Fable),
            Some(IneligibleReason::FableWeeklyExhausted)
        );
    }

    #[test]
    fn reset_fable_window_no_longer_excludes_fable() {
        // is_active=true but the weekly window has already reset → carries no
        // constraint (reset-aware via effective_utilization).
        let mut a = account("a");
        a.scoped_limits = vec![ScopedQuotaWindow {
            scope_label: "Fable".into(),
            window: QuotaWindow {
                utilization: 1.0,
                resets_at: at(NOW_SECS - 1),
                fetched_at: at(NOW_SECS - 10),
                source: WindowSource::UsagePoll,
            },
            severity: LimitSeverity::Critical,
            is_active: true,
        }];
        assert_eq!(
            gate_scoped(&a, &params(), now(), false, false, RequestScope::Fable),
            None,
            "a reset Fable window stops excluding Fable"
        );
    }

    /// An account whose Fable weekly bucket is at 76% / `warning` /
    /// `is_active: true` — the live-evidence regression shape: `is_active` marks
    /// the representative limit, NOT an exhausted one, so ~24% headroom remains.
    fn fable_headroom(id_: &str) -> AccountSnapshot {
        let mut a = account(id_);
        a.scoped_limits = vec![ScopedQuotaWindow {
            scope_label: "Fable".into(),
            window: window(0.76, 24 * HOUR),
            severity: LimitSeverity::Warning,
            is_active: true,
        }];
        a
    }

    #[test]
    fn fable_request_is_allowed_to_an_account_with_fable_headroom() {
        // Regression (W2 `is_active` misread): a Fable weekly at
        // 76%/warning/is_active=true is NOT exhausted — `is_active` means "the
        // governing limit", not "rejecting". The account keeps ~24% headroom, so
        // a Fable request must be ALLOWED (gate returns None).
        let ok = fable_headroom("ok");
        assert!(
            !ok.fable_weekly_exhausted(now(), 0.98),
            "76%/warning/is_active is NOT exhausted — it still has headroom"
        );
        assert_eq!(
            gate_scoped(&ok, &params(), now(), false, false, RequestScope::Fable),
            None,
            "a Fable request to a 76% (is_active, warning) account is eligible"
        );
        // Contrast: a genuinely exhausted Fable account (100%/Critical/is_active)
        // IS still excluded from Fable routing.
        let dead = fable_critical("dead");
        assert!(dead.fable_weekly_exhausted(now(), 0.98));
        assert_eq!(
            gate_scoped(&dead, &params(), now(), false, false, RequestScope::Fable),
            Some(IneligibleReason::FableWeeklyExhausted),
            "a util=1.0/Critical/is_active Fable account stays excluded"
        );
    }

    #[test]
    fn pick_scoped_fable_avoids_a_fable_dead_current_non_fable_stays() {
        // a is the sticky current and Fable-dead (scoped cooldown) but
        // account-wide clean; b is clean. A non-Fable request stays on a; a
        // Fable request switches off a to b.
        let a = fable_cooling("a", 60);
        let b = account("b");
        let snap = pool(vec![a, b], Some("a"));
        assert_eq!(
            pick_scoped(&snap, &params(), None, now(), RequestScope::NonFable),
            Decision::Stay,
            "non-Fable stays on the sticky current"
        );
        assert_eq!(
            pick_scoped(&snap, &params(), None, now(), RequestScope::Fable),
            Decision::Switch { to: id("b") },
            "Fable switches off the Fable-dead current"
        );
    }

    #[test]
    fn pick_scoped_fable_exhausts_when_every_account_is_fable_dead() {
        // The only account is Fable-critical: a Fable request exhausts, while a
        // non-Fable request still selects it.
        let a = fable_critical("a");
        let snap = pool(vec![a], None);
        assert!(matches!(
            pick_scoped(&snap, &params(), None, now(), RequestScope::Fable),
            Decision::Exhausted { .. }
        ));
        assert_eq!(
            pick_scoped(&snap, &params(), None, now(), RequestScope::NonFable),
            Decision::Switch { to: id("a") },
            "non-Fable capacity is untouched"
        );
    }

    fn fable_bucket(id_: &str, utilization: f64, resets_in_secs: u64) -> AccountSnapshot {
        let mut a = account(id_);
        a.scoped_limits = vec![ScopedQuotaWindow {
            scope_label: "Fable".into(),
            window: window(utilization, resets_in_secs),
            severity: LimitSeverity::Normal,
            is_active: false,
        }];
        a
    }

    /// Live defect 2026-07-16: two subscriptions — the fable lane camped a
    /// 93%-remaining bucket resetting in 1d14h while a FULL bucket resetting
    /// in 3h went unburned and expired. Use-it-or-lose-it must win: the
    /// hyperbolic urgency (needed burn rate) makes the imminent reset
    /// decisive over stickiness.
    #[test]
    fn fable_lane_burns_the_soon_resetting_full_bucket_first() {
        let far = fable_bucket("far", 0.07, 38 * HOUR);
        let soon = fable_bucket("soon", 0.0, 3 * HOUR);
        // `far` is the account current AND the sticky fable current.
        let mut snap = pool(vec![far, soon], Some("far"));
        snap.fable_current.insert(BackendGroup::Claude, id("far"));
        assert_eq!(
            pick_scoped(&snap, &params(), None, now(), RequestScope::Fable),
            Decision::Switch { to: id("soon") },
            "a full bucket resetting in 3h outranks a camped 93% bucket at 1d14h"
        );
        assert_eq!(
            pick_scoped(&snap, &params(), None, now(), RequestScope::NonFable),
            Decision::Stay,
            "the main lane is untouched by fable perishability"
        );
    }

    /// Issue #121: with no clearly-better bucket, the fable lane anchors on
    /// the account-wide current — a COLD fable bucket included — and the
    /// seeded anchor materializes as a Switch so `fable_current` commits.
    #[test]
    fn fable_lane_seeds_its_slot_from_the_account_current_when_cold() {
        // Both accounts fable-cold: no perishability evidence anywhere.
        let a = account("a");
        let b = account("b");
        let snap = pool(vec![a, b], Some("b"));
        assert_eq!(
            pick_scoped(&snap, &params(), None, now(), RequestScope::Fable),
            Decision::Switch { to: id("b") },
            "empty fable slot seeds from the account current, not the ranker's pick"
        );
        // Once the slot points at the anchor, the lane stays.
        let mut snap = pool(vec![account("a"), account("b")], Some("b"));
        snap.fable_current.insert(BackendGroup::Claude, id("b"));
        assert_eq!(
            pick_scoped(&snap, &params(), None, now(), RequestScope::Fable),
            Decision::Stay
        );
    }

    /// Review M2 (PR #124): a COLD challenger must never hunt a known
    /// anchor — no live Fable window means a phantom full bucket, not
    /// evidence. The half-used far-reset anchor stays.
    #[test]
    fn fable_lane_never_hunts_a_cold_challenger_over_a_known_anchor() {
        // Anchor: known mid-utilization, long runway (score ≈ 0.56).
        let anchor = fable_bucket("anchor", 0.50, 6 * 24 * HOUR);
        // Challenger: fable-COLD (phantom full headroom, score ≈ 0.9+).
        let cold = account("cold");
        let mut snap = pool(vec![anchor, cold], Some("anchor"));
        snap.fable_current
            .insert(BackendGroup::Claude, id("anchor"));
        assert_eq!(
            pick_scoped(&snap, &params(), None, now(), RequestScope::Fable),
            Decision::Stay,
            "cold is held by anchoring, never hunted"
        );
        // A KNOWN soon-resetting challenger still wins (the M2 guard must
        // not disable real perishability evidence).
        let anchor = fable_bucket("anchor", 0.50, 6 * 24 * HOUR);
        let soon = fable_bucket("soon", 0.0, 3 * HOUR);
        let mut snap = pool(vec![anchor, soon], Some("anchor"));
        snap.fable_current
            .insert(BackendGroup::Claude, id("anchor"));
        assert_eq!(
            pick_scoped(&snap, &params(), None, now(), RequestScope::Fable),
            Decision::Switch { to: id("soon") }
        );
    }

    /// Paused is absolute for the fable lane too (issue #121): a paused
    /// anchor is never followed — the lane picks among the remaining
    /// eligible accounts.
    #[test]
    fn fable_lane_never_follows_a_paused_anchor() {
        let mut a = account("a");
        a.paused = true;
        let b = account("b");
        let mut snap = pool(vec![a, b], Some("a"));
        snap.fable_current.insert(BackendGroup::Claude, id("a"));
        assert_eq!(
            pick_scoped(&snap, &params(), None, now(), RequestScope::Fable),
            Decision::Switch { to: id("b") },
            "a paused fable sticky current is abandoned at the next evaluation"
        );
    }

    #[test]
    fn blocking_reason_covers_the_fable_scoped_states() {
        let cooling = fable_cooling("a", 192);
        assert_eq!(
            blocking_reason(
                &cooling,
                IneligibleReason::FableCoolingDown,
                &params(),
                now()
            ),
            "fable cooldown 3m12s"
        );
        let critical = fable_critical("b");
        assert_eq!(
            blocking_reason(
                &critical,
                IneligibleReason::FableWeeklyExhausted,
                &params(),
                now()
            ),
            "fable 100% critical"
        );
    }

    // ---- exhaustion classification (issue #71): transient park ≠ quota ----

    #[test]
    fn burst_fable_cooldowns_classify_as_cooldown_blocked_with_seconds_expiry() {
        // Two fable-parked accounts (8s + 5s) with 5h/7d resets HOURS away:
        // the answer must be the 5s park, never a window-reset-scale value.
        let mut a = fable_cooling("a", 8);
        a.five_hour = Some(window(0.10, 2 * HOUR));
        let mut b = fable_cooling("b", 5);
        b.five_hour = Some(window(0.10, 2 * HOUR));
        let snap = pool(vec![a, b], Some("a"));
        let (kind, eligible) =
            classify_exhaustion(&snap, &params(), None, RequestScope::Fable, now());
        assert_eq!(eligible, 2);
        match kind {
            ExhaustionKind::CooldownBlocked {
                min_expiry,
                upstream_mandated,
            } => {
                assert_eq!(min_expiry, Duration::from_secs(5));
                assert!(!upstream_mandated, "scoped parks are our own heuristic");
            }
            other => panic!("expected CooldownBlocked, got {other:?}"),
        }
    }

    #[test]
    fn window_capped_pool_classifies_as_window_blocked() {
        let mut a = account("a");
        a.seven_day = Some(window(1.0, 24 * HOUR));
        let mut b = account("b");
        b.seven_day = Some(window(1.0, 24 * HOUR));
        let snap = pool(vec![a, b], Some("a"));
        let (kind, eligible) =
            classify_exhaustion(&snap, &params(), None, RequestScope::NonFable, now());
        assert_eq!(kind, ExhaustionKind::WindowBlocked);
        assert_eq!(
            eligible, 2,
            "window-blocked speaks for the whole in-group pool"
        );
    }

    #[test]
    fn mixed_pool_recovers_when_the_park_lapses_so_cooldown_wins() {
        // One account fable-parked 8s out, one fable-weekly-capped for days:
        // the pool is usable again in 8s — CooldownBlocked, counting only the
        // parked account.
        let cooled = fable_cooling("a", 8);
        let capped = fable_critical("b");
        let snap = pool(vec![cooled, capped], Some("a"));
        let (kind, eligible) =
            classify_exhaustion(&snap, &params(), None, RequestScope::Fable, now());
        assert_eq!(eligible, 1);
        match kind {
            ExhaustionKind::CooldownBlocked {
                min_expiry,
                upstream_mandated,
            } => {
                assert_eq!(min_expiry, Duration::from_secs(8));
                assert!(!upstream_mandated);
            }
            other => panic!("expected CooldownBlocked, got {other:?}"),
        }
    }

    #[test]
    fn upstream_retry_after_park_classifies_as_mandated_cooldown() {
        // An upstream-mandated park (retry-after header) is still
        // CooldownBlocked, but flagged mandated: the proxy answers 429 with
        // exactly that wait — upstream's own number is honest.
        let mut a = account("a");
        a.cooldown_until = Some(at(NOW_SECS + 1800));
        a.cooldown_source = Some(CooldownSource::RetryAfter);
        let snap = pool(vec![a], Some("a"));
        let (kind, eligible) =
            classify_exhaustion(&snap, &params(), None, RequestScope::NonFable, now());
        assert_eq!(eligible, 1);
        match kind {
            ExhaustionKind::CooldownBlocked {
                min_expiry,
                upstream_mandated,
            } => {
                assert_eq!(min_expiry, Duration::from_secs(1800));
                assert!(upstream_mandated);
            }
            other => panic!("expected CooldownBlocked, got {other:?}"),
        }
    }
}
