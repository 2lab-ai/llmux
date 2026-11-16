//! Subscription plan per account (`.prd/25-account-plan-tier.md`).
//!
//! Display/consumer data only — the scheduler never reads it. Two upstream
//! sources, both measured on real accounts 2026-10-26:
//!
//! - Claude: `organization.rate_limit_tier` of `GET /api/oauth/profile`
//!   (`default_claude_max_20x`, `default_claude_max_5x`, …).
//! - Codex: the `chatgpt_plan_type` claim inside the ChatGPT access token's
//!   `https://api.openai.com/auth` object (`pro` = $200, `promax` = $500).
//!
//! A bare `max` (the credential file's `subscriptionType`) is never turned
//! into `max5` / `max20` — that is exactly the ambiguity this module removes.
//!
//! Whether an account is a personal subscription or a seat in a team
//! ([`AccountType`], `.prd/25` amendment 2026-11-16) comes from the same two
//! sources: Claude's `organization.organization_type`, Codex's plan word. The
//! label alone cannot say it: a Claude Team seat can carry
//! `default_claude_max_5x` (Claude Code itself checks for exactly that pair),
//! so it reads `max5` like a personal Max x5 subscription.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Whether an account is the owner's own subscription or a seat in a team
/// organization. Display/consumer data only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountType {
    /// A personal subscription (Claude Max / Pro, ChatGPT Free / Plus / Pro).
    Personal,
    /// A seat in a team organization (Claude Team / Enterprise, ChatGPT
    /// Team / Business / Enterprise / Edu).
    Team,
}

impl AccountType {
    /// The wire and display word: `personal` / `team`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Team => "team",
        }
    }
}

/// The account type of a Claude `organization.organization_type` — the same
/// mapping Claude Code uses for its own subscription type (`claude_max` →
/// max, `claude_pro` → pro, `claude_team` → team, `claude_enterprise` →
/// enterprise). `None` for any other value: never a guess.
pub fn claude_account_type(organization_type: &str) -> Option<AccountType> {
    match organization_type.trim().to_ascii_lowercase().as_str() {
        "claude_max" | "claude_pro" => Some(AccountType::Personal),
        "claude_team" | "claude_enterprise" => Some(AccountType::Team),
        _ => None,
    }
}

/// The account type of a Codex `plan_type`. ChatGPT's workspace plans
/// (`team`, `business`, `enterprise`, `edu`, …) are team seats; the
/// individual plans (`free`, `go`, `plus`, `pro`, `promax`) are personal.
/// `None` for a word that names neither.
pub fn codex_account_type(plan_type: &str) -> Option<AccountType> {
    let plan = plan_type.trim().to_ascii_lowercase();
    if ["team", "business", "enterprise", "edu", "k12"]
        .iter()
        .any(|word| plan.contains(word))
    {
        return Some(AccountType::Team);
    }
    matches!(plan.as_str(), "free" | "go" | "plus" | "pro" | "promax")
        .then_some(AccountType::Personal)
}

/// One-phrase plan summary: `max20 personal`, `max5 team`, `team` (a label
/// that already says the type is not repeated); either half alone when only
/// one is known; `None` when neither is. The one formatter behind the
/// `llmux status` line, the TUI detail pane and the account modal.
pub fn plan_summary(label: Option<&str>, kind: Option<&str>) -> Option<String> {
    match (label, kind) {
        (Some(label), Some(kind)) if label == kind => Some(label.to_string()),
        (Some(label), Some(kind)) => Some(format!("{label} {kind}")),
        (Some(label), None) => Some(label.to_string()),
        (None, Some(kind)) => Some(kind.to_string()),
        (None, None) => None,
    }
}

/// Short display label of a Claude login: the `rate_limit_tier` label when it
/// names a plan ([`claude_label`]), else the plan word of the
/// `organization_type` — `team`, `enterprise`, `pro`, `max` — so a team seat
/// whose tier names no Max plan still reads as a plan. `None` when neither
/// names one.
pub fn claude_plan_label(
    rate_limit_tier: Option<&str>,
    organization_type: Option<&str>,
) -> Option<String> {
    rate_limit_tier.and_then(claude_label).or_else(|| {
        let word = match organization_type?.trim().to_ascii_lowercase().as_str() {
            "claude_team" => "team",
            "claude_enterprise" => "enterprise",
            "claude_pro" => "pro",
            "claude_max" => "max",
            _ => return None,
        };
        Some(word.to_string())
    })
}

/// Short display label of a Claude `rate_limit_tier`: `max20`, `max5`, `pro`.
/// `None` for a value that does not name a plan exactly.
pub fn claude_label(rate_limit_tier: &str) -> Option<String> {
    let tier = rate_limit_tier.trim().to_ascii_lowercase();
    if tier.contains("max_20x") || tier.contains("max20x") {
        return Some("max20".into());
    }
    if tier.contains("max_5x") || tier.contains("max5x") {
        return Some("max5".into());
    }
    if tier.ends_with("_pro") || tier == "pro" {
        return Some("pro".into());
    }
    None
}

/// Short display label of a Codex `plan_type`: the plan word itself,
/// lowercased (`pro`, `promax`, `plus`, `team`, …). `None` for an empty or
/// non-word value — never a guess.
pub fn codex_label(plan_type: &str) -> Option<String> {
    let plan = plan_type.trim().to_ascii_lowercase();
    let word = !plan.is_empty()
        && plan.len() <= 16
        && plan.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    word.then_some(plan)
}

/// The `chatgpt_plan_type` claim of a ChatGPT access token (a JWT). Decoded
/// without a signature check: the value is display-only and the token is the
/// one llmux already holds for the account. `None` when the token is not a
/// JWT or carries no plan claim.
pub fn codex_plan_type(access_token: &str) -> Option<String> {
    let payload = access_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let plan = claims
        .get("https://api.openai.com/auth")?
        .get("chatgpt_plan_type")?
        .as_str()?
        .trim()
        .to_string();
    (!plan.is_empty()).then_some(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: serde_json::Value) -> String {
        let enc = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        format!(
            "{}.{}.sig",
            enc(br#"{"alg":"RS256"}"#),
            enc(claims.to_string().as_bytes())
        )
    }

    #[test]
    fn claude_labels_name_the_plan_exactly() {
        assert_eq!(
            claude_label("default_claude_max_20x").as_deref(),
            Some("max20")
        );
        assert_eq!(
            claude_label("default_claude_max_5x").as_deref(),
            Some("max5")
        );
        assert_eq!(claude_label("claude_max_20x").as_deref(), Some("max20"));
        assert_eq!(claude_label("default_claude_pro").as_deref(), Some("pro"));
        // a bare "max" never becomes max5 / max20
        assert_eq!(claude_label("max"), None);
        assert_eq!(claude_label(""), None);
    }

    /// `.prd/25` amendment 2026-11-16: personal vs team follows the
    /// organization type, never the rate tier.
    #[test]
    fn claude_account_type_follows_the_organization_type() {
        assert_eq!(
            claude_account_type("claude_max"),
            Some(AccountType::Personal)
        );
        assert_eq!(
            claude_account_type("claude_pro"),
            Some(AccountType::Personal)
        );
        assert_eq!(claude_account_type("claude_team"), Some(AccountType::Team));
        assert_eq!(
            claude_account_type(" Claude_Enterprise "),
            Some(AccountType::Team)
        );
        assert_eq!(claude_account_type("api"), None);
        assert_eq!(claude_account_type(""), None);
    }

    /// A team seat with a Max-sized tier keeps the capacity label (`max5`,
    /// what consumers weigh); a tier that names no plan falls back to the
    /// organization's plan word instead of no label at all.
    #[test]
    fn claude_plan_label_prefers_the_tier_then_the_organization() {
        assert_eq!(
            claude_plan_label(Some("default_claude_max_5x"), Some("claude_team")).as_deref(),
            Some("max5")
        );
        assert_eq!(
            claude_plan_label(Some("default_raven"), Some("claude_team")).as_deref(),
            Some("team")
        );
        assert_eq!(
            claude_plan_label(None, Some("claude_enterprise")).as_deref(),
            Some("enterprise")
        );
        assert_eq!(
            claude_plan_label(Some("default_raven"), Some("claude_max")).as_deref(),
            Some("max")
        );
        assert_eq!(
            claude_plan_label(Some("default_claude_max_20x"), None).as_deref(),
            Some("max20")
        );
        assert_eq!(claude_plan_label(Some("default_raven"), None), None);
        assert_eq!(claude_plan_label(None, Some("api")), None);
    }

    #[test]
    fn codex_account_type_splits_workspace_plans_from_individual_ones() {
        for personal in ["free", "go", "plus", "pro", "promax", " Pro "] {
            assert_eq!(
                codex_account_type(personal),
                Some(AccountType::Personal),
                "{personal}"
            );
        }
        for team in [
            "team",
            "business",
            "enterprise",
            "edu",
            "self_serve_business",
        ] {
            assert_eq!(codex_account_type(team), Some(AccountType::Team), "{team}");
        }
        assert_eq!(codex_account_type("unknown"), None);
        assert_eq!(codex_account_type(""), None);
    }

    #[test]
    fn plan_summary_joins_label_and_type_without_repeating() {
        assert_eq!(
            plan_summary(Some("max5"), Some("team")).as_deref(),
            Some("max5 team")
        );
        assert_eq!(
            plan_summary(Some("team"), Some("team")).as_deref(),
            Some("team")
        );
        assert_eq!(plan_summary(Some("max20"), None).as_deref(), Some("max20"));
        assert_eq!(
            plan_summary(None, Some("personal")).as_deref(),
            Some("personal")
        );
        assert_eq!(plan_summary(None, None), None);
    }

    #[test]
    fn account_type_wire_words() {
        assert_eq!(AccountType::Personal.as_str(), "personal");
        assert_eq!(AccountType::Team.as_str(), "team");
        assert_eq!(
            serde_json::to_value(AccountType::Team).unwrap(),
            serde_json::json!("team")
        );
    }

    #[test]
    fn codex_labels_pass_the_plan_word_through() {
        assert_eq!(codex_label("pro").as_deref(), Some("pro"));
        assert_eq!(codex_label("ProMax").as_deref(), Some("promax"));
        assert_eq!(codex_label(" "), None);
        assert_eq!(codex_label("not a plan word"), None);
    }

    #[test]
    fn codex_plan_type_reads_the_auth_claim() {
        let token = jwt(serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_plan_type": "promax", "chatgpt_account_id": "a" }
        }));
        assert_eq!(codex_plan_type(&token).as_deref(), Some("promax"));
    }

    #[test]
    fn codex_plan_type_is_none_without_a_claim_or_a_jwt() {
        assert_eq!(codex_plan_type("opaque-token"), None);
        assert_eq!(
            codex_plan_type(&jwt(serde_json::json!({ "sub": "x" }))),
            None
        );
        assert_eq!(
            codex_plan_type(&jwt(serde_json::json!({
                "https://api.openai.com/auth": { "chatgpt_plan_type": "" }
            }))),
            None
        );
        assert_eq!(codex_plan_type("a.!!!.c"), None);
    }
}
