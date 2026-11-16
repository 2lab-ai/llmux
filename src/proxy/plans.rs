//! Daemon-learned subscription plan per account (`.prd/25-account-plan-tier.md`).
//!
//! Display/consumer data only: the scheduler never reads it. The store is
//! filled by [`refresh_plans`] — once at daemon start (off the readiness path),
//! then every [`PLAN_RETRY`] while a plan-capable account has no plan yet (an
//! expired token, a failed read, a fresh login) and every [`PLAN_TICK`] once all
//! are known — and read by `/llmux/status` and the dashboard document. It is
//! daemon state, not config: nothing is persisted, a restart re-learns it.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::AccountCredential;

/// How often the daemon re-reads every account's plan once all are known.
pub const PLAN_TICK: Duration = Duration::from_secs(6 * 3600);
/// The next pass while some plan-capable account has no plan yet.
pub const PLAN_RETRY: Duration = Duration::from_secs(10 * 60);
/// A Claude token this close to expiry is not used for a profile read: the
/// read would 401, and the background refresh renews it first.
const TOKEN_MARGIN_MS: u64 = 60_000;

/// One account's plan as reported upstream, with its short display label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountPlan {
    /// Short display label (`max20`, `max5`, `pro`, `promax`, …); `None` when
    /// the raw value does not name a plan exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Claude: `organization.rate_limit_tier`, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_tier: Option<String>,
    /// Codex: the token's `chatgpt_plan_type`, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    /// Personal subscription or team seat (`.prd/25` amendment 2026-11-16);
    /// `None` when the upstream value names neither. Additive: an older
    /// daemon's document has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_type: Option<crate::auth::plan::AccountType>,
    /// Claude: `organization.organization_type`, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_type: Option<String>,
    /// Claude: Anthropic's own name for the plan (`organization.plan_display_name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_name: Option<String>,
    /// Claude: the team seat tier (`organization.seat_tier`), verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat_tier: Option<String>,
}

impl AccountPlan {
    /// A Claude plan known only by its `rate_limit_tier` (no organization
    /// data: the account type stays unknown).
    pub fn claude(rate_limit_tier: &str) -> Self {
        Self {
            label: crate::auth::plan::claude_label(rate_limit_tier),
            rate_limit_tier: Some(rate_limit_tier.to_string()),
            plan_type: None,
            account_type: None,
            organization_type: None,
            plan_name: None,
            seat_tier: None,
        }
    }

    /// A Claude plan from one profile read: the tier, the organization type
    /// (→ label fallback and account type), the display name and the seat.
    /// `None` when the profile reports neither a tier nor an organization
    /// type — nothing to show, and the store keeps the last known plan.
    pub fn from_claude_profile(profile: &crate::auth::profile::Profile) -> Option<Self> {
        let tier = profile.rate_limit_tier.as_deref();
        let org_type = profile.organization_type.as_deref();
        if tier.is_none() && org_type.is_none() {
            return None;
        }
        Some(Self {
            label: crate::auth::plan::claude_plan_label(tier, org_type),
            rate_limit_tier: profile.rate_limit_tier.clone(),
            plan_type: None,
            account_type: org_type.and_then(crate::auth::plan::claude_account_type),
            organization_type: profile.organization_type.clone(),
            plan_name: profile.plan_display_name.clone(),
            seat_tier: profile.seat_tier.clone(),
        })
    }

    pub fn codex(plan_type: &str) -> Self {
        Self {
            label: crate::auth::plan::codex_label(plan_type),
            rate_limit_tier: None,
            plan_type: Some(plan_type.to_string()),
            account_type: crate::auth::plan::codex_account_type(plan_type),
            organization_type: None,
            plan_name: None,
            seat_tier: None,
        }
    }
}

/// Plans by account name.
#[derive(Debug, Default)]
pub struct PlanStore {
    inner: RwLock<HashMap<String, AccountPlan>>,
}

impl PlanStore {
    pub fn all(&self) -> HashMap<String, AccountPlan> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn get(&self, account: &str) -> Option<AccountPlan> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(account)
            .cloned()
    }

    /// Replace the store with one pass's results. An account the pass could
    /// not read (`None`) keeps its last known plan; an account no longer in
    /// the pool drops out.
    pub fn apply(&self, pass: Vec<(String, Option<AccountPlan>)>) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut next = HashMap::with_capacity(pass.len());
        for (name, plan) in pass {
            if let Some(plan) = plan.or_else(|| guard.get(&name).cloned()) {
                next.insert(name, plan);
            }
        }
        *guard = next;
    }
}

/// Where one account's plan comes from, decided from its credential alone.
#[derive(Debug, PartialEq)]
pub enum PlanSource {
    /// Claude oauth: fetch the profile with this (still valid) access token.
    ClaudeProfile { access_token: String },
    /// Claude oauth whose token is expired or about to: unknown this pass
    /// (the store keeps the last plan; the next pass retries after refresh).
    NotNow,
    /// Codex: the plan is already in hand (token claim), or `None`.
    Known(Option<AccountPlan>),
    /// A provider without plans (grok, openrouter, gemini, api keys).
    None,
}

/// The plan source for one credential at `now_ms` (epoch ms).
pub fn plan_source(credential: &AccountCredential, now_ms: u64) -> PlanSource {
    match credential {
        // `0` = unknown expiry, treated as expired (credentials.rs).
        AccountCredential::Oauth {
            access_token,
            expires_at_ms,
            ..
        } => {
            if *expires_at_ms > now_ms.saturating_add(TOKEN_MARGIN_MS) {
                PlanSource::ClaudeProfile {
                    access_token: access_token.clone(),
                }
            } else {
                PlanSource::NotNow
            }
        }
        AccountCredential::Codex { access_token, .. } => PlanSource::Known(
            crate::auth::plan::codex_plan_type(access_token).map(|p| AccountPlan::codex(&p)),
        ),
        _ => PlanSource::None,
    }
}

/// One pass over the pool: every account's plan, fetched or decoded. Errors
/// and expired tokens read as "unknown this pass" (the store keeps the last
/// known value); a profile with neither a `rate_limit_tier` nor an
/// `organization_type` reads the same.
/// Returns how many plan-capable accounts (Claude oauth, Codex) still have no
/// plan after the pass — the caller retries soon while that is non-zero.
pub async fn refresh_plans(
    store: &PlanStore,
    pool: &crate::scheduler::AccountPool,
    client: &reqwest::Client,
    upstream: &str,
) -> usize {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let mut pass = Vec::new();
    let mut capable = Vec::new();
    for account in pool.snapshot().accounts {
        let Some(credential) = pool.credential(&account.id) else {
            continue;
        };
        let name = account.id.0.clone();
        let plan = match plan_source(&credential, now_ms) {
            PlanSource::None => None,
            PlanSource::Known(plan) => {
                capable.push(name.clone());
                plan
            }
            PlanSource::NotNow => {
                capable.push(name.clone());
                None
            }
            PlanSource::ClaudeProfile { access_token } => {
                capable.push(name.clone());
                match crate::auth::profile::fetch_profile(client, upstream, &access_token).await {
                    Ok(profile) => AccountPlan::from_claude_profile(&profile),
                    Err(err) => {
                        tracing::info!(account = %name, status = ?err.http_status(), "plan: profile read failed; retrying on the next pass");
                        None
                    }
                }
            }
        };
        pass.push((name, plan));
    }
    store.apply(pass);
    missing(store, &capable)
}

/// How many of `capable` have no plan in `store`.
fn missing(store: &PlanStore, capable: &[String]) -> usize {
    let known = store.all();
    capable
        .iter()
        .filter(|name| !known.contains_key(*name))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oauth(expires_at_ms: u64) -> AccountCredential {
        AccountCredential::Oauth {
            account_uuid: String::new(),
            access_token: "at".into(),
            refresh_token: "rt".into(),
            expires_at_ms,
            tier: None,
            last_refresh_ms: None,
            login_expires_at_ms: None,
        }
    }

    /// Review P1: an expired (or unknown-expiry) Claude token is never sent to
    /// the profile endpoint — that pass reads "not now", not a 401.
    #[test]
    fn an_expired_claude_token_is_not_used_for_a_profile_read() {
        let now = 1_800_000_000_000u64;
        assert_eq!(
            plan_source(&oauth(now + 3_600_000), now),
            PlanSource::ClaudeProfile {
                access_token: "at".into()
            }
        );
        assert_eq!(plan_source(&oauth(now + 30_000), now), PlanSource::NotNow);
        assert_eq!(plan_source(&oauth(now - 1), now), PlanSource::NotNow);
        assert_eq!(plan_source(&oauth(0), now), PlanSource::NotNow);
    }

    #[test]
    fn missing_counts_plan_capable_accounts_without_a_plan() {
        let store = PlanStore::default();
        store.apply(vec![(
            "claude:a".into(),
            Some(AccountPlan::claude("default_claude_max_20x")),
        )]);
        let capable = vec!["claude:a".to_string(), "claude:b".to_string()];
        assert_eq!(missing(&store, &capable), 1);
        assert_eq!(missing(&store, &capable[..1]), 0);
    }

    #[test]
    fn labels_follow_the_upstream_value() {
        let claude = AccountPlan::claude("default_claude_max_20x");
        assert_eq!(claude.label.as_deref(), Some("max20"));
        assert_eq!(
            claude.rate_limit_tier.as_deref(),
            Some("default_claude_max_20x")
        );
        assert_eq!(claude.plan_type, None);
        let codex = AccountPlan::codex("promax");
        assert_eq!(codex.label.as_deref(), Some("promax"));
        assert_eq!(codex.plan_type.as_deref(), Some("promax"));
    }

    #[test]
    fn a_failed_read_keeps_the_last_plan_and_a_removed_account_drops_out() {
        let store = PlanStore::default();
        store.apply(vec![
            (
                "claude:a".into(),
                Some(AccountPlan::claude("default_claude_max_20x")),
            ),
            ("codex:b".into(), Some(AccountPlan::codex("pro"))),
        ]);
        // next pass: a's read failed, b was removed, c is new but unknown
        store.apply(vec![("claude:a".into(), None), ("claude:c".into(), None)]);
        let all = store.all();
        assert_eq!(all.len(), 1);
        assert_eq!(all["claude:a"].label.as_deref(), Some("max20"));
        assert_eq!(store.get("codex:b"), None);
        assert_eq!(store.get("claude:c"), None);
    }

    #[test]
    fn serializes_only_the_fields_it_knows() {
        let json = serde_json::to_value(AccountPlan::codex("pro")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "label": "pro", "plan_type": "pro", "account_type": "personal" })
        );
        let json = serde_json::to_value(AccountPlan::claude("default_claude_max_20x")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "label": "max20", "rate_limit_tier": "default_claude_max_20x" })
        );
    }

    /// An older daemon's document carries no account type: it still parses.
    #[test]
    fn an_older_document_without_the_type_still_parses() {
        let plan: AccountPlan =
            serde_json::from_value(serde_json::json!({ "label": "max20" })).unwrap();
        assert_eq!(plan.label.as_deref(), Some("max20"));
        assert_eq!(plan.account_type, None);
    }

    fn profile(tier: Option<&str>, org_type: Option<&str>) -> crate::auth::profile::Profile {
        crate::auth::profile::Profile {
            account_uuid: "u".into(),
            email: "e@x.com".into(),
            tier: None,
            display_name: None,
            org_name: None,
            org_uuid: None,
            has_claude_max: None,
            has_claude_pro: None,
            rate_limit_tier: tier.map(String::from),
            organization_type: org_type.map(String::from),
            plan_display_name: Some("Ultra".into()),
            seat_tier: None,
        }
    }

    /// `.prd/25` amendment 2026-11-16: a Claude Team seat with a Max x5
    /// tier keeps the `max5` label consumers weigh, and says `team`; a
    /// personal Max x20 says `personal`; a team tier that names no plan
    /// still gets a label.
    #[test]
    fn a_claude_profile_names_the_plan_and_whether_it_is_a_team_seat() {
        let team = AccountPlan::from_claude_profile(&profile(
            Some("default_claude_max_5x"),
            Some("claude_team"),
        ))
        .unwrap();
        assert_eq!(team.label.as_deref(), Some("max5"));
        assert_eq!(
            team.account_type,
            Some(crate::auth::plan::AccountType::Team)
        );
        assert_eq!(team.organization_type.as_deref(), Some("claude_team"));
        assert_eq!(team.plan_name.as_deref(), Some("Ultra"));

        let personal = AccountPlan::from_claude_profile(&profile(
            Some("default_claude_max_20x"),
            Some("claude_max"),
        ))
        .unwrap();
        assert_eq!(personal.label.as_deref(), Some("max20"));
        assert_eq!(
            personal.account_type,
            Some(crate::auth::plan::AccountType::Personal)
        );

        let raven =
            AccountPlan::from_claude_profile(&profile(Some("default_raven"), Some("claude_team")))
                .unwrap();
        assert_eq!(raven.label.as_deref(), Some("team"));
        assert_eq!(raven.rate_limit_tier.as_deref(), Some("default_raven"));

        // tier only (an older profile shape): label, no type
        let bare = AccountPlan::from_claude_profile(&profile(Some("default_claude_max_20x"), None))
            .unwrap();
        assert_eq!(bare.label.as_deref(), Some("max20"));
        assert_eq!(bare.account_type, None);

        // organization type only (no tier): still a plan, labelled by the
        // organization's plan word (review P2)
        let org_only =
            AccountPlan::from_claude_profile(&profile(None, Some("claude_team"))).unwrap();
        assert_eq!(org_only.label.as_deref(), Some("team"));
        assert_eq!(
            org_only.account_type,
            Some(crate::auth::plan::AccountType::Team)
        );
        assert_eq!(org_only.rate_limit_tier, None);

        assert_eq!(AccountPlan::from_claude_profile(&profile(None, None)), None);
    }

    #[test]
    fn a_codex_workspace_plan_is_a_team_seat() {
        assert_eq!(
            AccountPlan::codex("team").account_type,
            Some(crate::auth::plan::AccountType::Team)
        );
        assert_eq!(
            AccountPlan::codex("promax").account_type,
            Some(crate::auth::plan::AccountType::Personal)
        );
    }
}
