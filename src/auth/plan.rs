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

use base64::Engine as _;

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
