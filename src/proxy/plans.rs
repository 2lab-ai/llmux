//! Daemon-learned subscription plan per account (`.prd/25-account-plan-tier.md`).
//!
//! Display/consumer data only: the scheduler never reads it. The store is
//! filled by [`refresh_plans`] — once at daemon start (off the readiness path)
//! and every [`PLAN_TICK`] after — and read by `/llmux/status` and the
//! dashboard document. It is daemon state, not config: nothing is persisted,
//! a restart re-learns it.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::AccountCredential;

/// How often the daemon re-reads every account's plan.
pub const PLAN_TICK: Duration = Duration::from_secs(6 * 3600);

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
}

impl AccountPlan {
    pub fn claude(rate_limit_tier: &str) -> Self {
        Self {
            label: crate::auth::plan::claude_label(rate_limit_tier),
            rate_limit_tier: Some(rate_limit_tier.to_string()),
            plan_type: None,
        }
    }

    pub fn codex(plan_type: &str) -> Self {
        Self {
            label: crate::auth::plan::codex_label(plan_type),
            rate_limit_tier: None,
            plan_type: Some(plan_type.to_string()),
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
pub enum PlanSource {
    /// Claude oauth: fetch the profile with this access token.
    ClaudeProfile { access_token: String },
    /// Codex: the plan is already in hand (token claim), or `None`.
    Known(Option<AccountPlan>),
}

/// The plan source for one credential. Accounts of providers without a plan
/// read `Known(None)`.
pub fn plan_source(credential: &AccountCredential) -> PlanSource {
    match credential {
        AccountCredential::Oauth { access_token, .. } => PlanSource::ClaudeProfile {
            access_token: access_token.clone(),
        },
        AccountCredential::Codex { access_token, .. } => PlanSource::Known(
            crate::auth::plan::codex_plan_type(access_token).map(|p| AccountPlan::codex(&p)),
        ),
        _ => PlanSource::Known(None),
    }
}

/// One pass over the pool: every account's plan, fetched or decoded. Errors
/// are logged and read as "unknown this pass" (the store keeps the last
/// known value); a profile without a `rate_limit_tier` reads the same.
pub async fn refresh_plans(
    store: &PlanStore,
    pool: &crate::scheduler::AccountPool,
    client: &reqwest::Client,
    upstream: &str,
) {
    let mut pass = Vec::new();
    for account in pool.snapshot().accounts {
        let Some(credential) = pool.credential(&account.id) else {
            continue;
        };
        let plan = match plan_source(&credential) {
            PlanSource::Known(plan) => plan,
            PlanSource::ClaudeProfile { access_token } => {
                match crate::auth::profile::fetch_profile(client, upstream, &access_token).await {
                    Ok(profile) => profile.rate_limit_tier.as_deref().map(AccountPlan::claude),
                    Err(err) => {
                        tracing::debug!(account = %account.id.0, error = %err, "plan: profile read failed");
                        None
                    }
                }
            }
        };
        pass.push((account.id.0.clone(), plan));
    }
    store.apply(pass);
}

#[cfg(test)]
mod tests {
    use super::*;

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
            serde_json::json!({ "label": "pro", "plan_type": "pro" })
        );
    }
}
