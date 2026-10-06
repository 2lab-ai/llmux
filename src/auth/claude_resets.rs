//! Claude usage-limit reset grants — the Anthropic wire layer of the
//! reset-credit controls (`.prd/20-claude-reset-grants.md`).
//!
//! claude.ai's Settings → Usage → "Limit resets" card is fed by the SAME
//! usage document llmux already polls, with one extra query flag:
//! `GET /api/oauth/usage?cedar_ember=1` adds a `cedar_ember` object (the
//! program's internal name) carrying the account's reset grants. Captured
//! live 2026-10-06 on claude.ai (`/api/organizations/{org}/usage?cedar_ember=1`)
//! and re-read through `api.anthropic.com/api/oauth/usage?cedar_ember=1` with
//! an llmux OAuth token.
//!
//! **The surface gate.** The endpoint answers `eligible:false,
//! ineligible_reason:"surface"` to a bare client and `"cli_version"` to a
//! Claude Code identity below the server's floor; with
//! `User-Agent: claude-cli/<current> (external, cli)` it returned the real
//! grants (probed 2026-10-06, four accounts). The version is therefore a
//! load-bearing input ([`DEFAULT_CLAUDE_CLI_VERSION`], overridable per config)
//! and a `cli_version` answer is surfaced verbatim, never hidden.
//!
//! Redemption is `POST /api/organizations/{org_uuid}/reset_rate_limits` with
//! `{program:"cedar_ember", grant_id, request_id}` (claude.ai bundle,
//! `shared-0-*.js`, 2026-10-06). The outcome enum and the `request_id`
//! idempotency key were read from that bundle; **no live consume has been
//! executed** — the parser is fixture-tested only.
//!
//! Same two rules as the codex layer: unknown is never zero, nothing leaks.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::codex_usage::{
    CodexUsageError, ConsumeResult, ResetCredit, ResetCredits, ResetEligibility, ResetOutcome,
    CONTROL_TIMEOUT,
};
use crate::scheduler::usage::{parse_usage_body, UsageSnapshot};

/// The Claude Code version llmux identifies as when it reads reset grants.
/// Probed 2026-10-06: `2.1.47` answered `cli_version`, `2.1.291` (the
/// installed CLI that day) answered with grants. Bump when the server floor
/// moves; `config.claude_cli_version` overrides without a rebuild.
pub const DEFAULT_CLAUDE_CLI_VERSION: &str = "2.1.291";

/// The `reset_type` llmux assigns to a mapped Claude grant. Codex rows carry
/// `codex_rate_limits`; this keeps the two distinguishable in one list.
pub const CLAUDE_RATE_LIMITS: &str = "claude_rate_limits";

/// The upstream program name the consume body names.
pub const PROGRAM: &str = "cedar_ember";

/// `User-Agent` for the grant read and the consume call. Only this header
/// moved the surface gate in the 2026-10-06 probes (`x-app`, `anthropic-beta`
/// and `anthropic-version` alone did not).
pub fn claude_cli_user_agent(version: &str) -> String {
    format!("claude-cli/{} (external, cli)", version.trim())
}

/// One reset grant as the server reports it. Every field is optional on the
/// wire side of the parser; [`parse_grant`] drops rows missing the three
/// load-bearing ones (`id`, `resets_left`, `ends_at`), matching the web
/// client's own validator.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResetGrant {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_total: Option<u64>,
    pub resets_left: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<String>,
    /// RFC3339 expiry of the grant — the "만료일" the settings card shows.
    pub ends_at: String,
    /// Limit windows one redemption clears (`five_hour`, `seven_day`, …).
    #[serde(default)]
    pub clears: Vec<String>,
    #[serde(default)]
    pub paused: bool,
    /// Server-computed "this grant can be used right now".
    #[serde(default)]
    pub usable_now: bool,
    /// `true` (the default) = the grant only applies while a limit is hit.
    #[serde(default = "default_true")]
    pub use_requires_limit: bool,
}

fn default_true() -> bool {
    true
}

/// The `cedar_ember` object: eligibility, the grants, and the two
/// account-level timestamps the card displays.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResetGrantStatus {
    /// `None` when the server omitted/nulled the flag (seen under
    /// rate-limiting) — unknown, not ineligible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligible: Option<bool>,
    /// Server enum: `surface`, `cli_version`, `tier`, `seat`, `no_grant`,
    /// `tenure`, `control`, `not_enrolled`, `plan_changed`, `config_off`,
    /// `mobile`, `other_experiment`, `unavailable` (claude.ai bundle). Passed
    /// through verbatim; unknown values are kept, not mapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ineligible_reason: Option<String>,
    #[serde(default)]
    pub at_limit: bool,
    #[serde(default)]
    pub grants: Vec<ResetGrant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_grant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekly_resets_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<String>,
}

impl ResetGrantStatus {
    /// Grants that still hold a reset and are not paused.
    fn live_grants(&self) -> impl Iterator<Item = &ResetGrant> {
        self.grants
            .iter()
            .filter(|g| !g.paused && g.resets_left > 0)
    }

    /// The account-level summary the daemon keeps on its control doc.
    pub fn eligibility(&self) -> ResetEligibility {
        ResetEligibility {
            eligible: self.eligible,
            ineligible_reason: self.ineligible_reason.clone(),
            weekly_resets_at: self.weekly_resets_at.clone(),
        }
    }

    /// Resets owned: the sum of `resets_left` over live grants — only when
    /// the server said the account is eligible. Ineligible or unknown
    /// eligibility is UNKNOWN (`None`), never `0`: an ineligible answer means
    /// "this surface may not see the grants", not "there are none".
    pub fn available_count(&self) -> Option<u64> {
        match self.eligible {
            Some(true) => Some(self.live_grants().map(|g| g.resets_left).sum()),
            _ => None,
        }
    }

    /// Resets usable right now (server-computed `usable_now`), same
    /// eligibility rule as [`Self::available_count`].
    pub fn applicable_count(&self) -> Option<u64> {
        match self.eligible {
            Some(true) => Some(self.live_grants().filter(|g| g.usable_now).count() as u64),
            _ => None,
        }
    }

    /// Project the grants onto the shared [`ResetCredits`] wire shape so the
    /// CLI listing, the TUI modal and the redemption gate work unchanged:
    /// `id`→`id`, `label`→`title`, `starts_at`→`granted_at`,
    /// `ends_at`→`expires_at`, `reset_type` = [`CLAUDE_RATE_LIMITS`], and a
    /// derived `status` — `available` exactly when the server says
    /// `usable_now` on a live grant (what [`ResetCredit::is_redeemable`]
    /// gates on), else `paused` / `spent` / `blocked`.
    pub fn to_credits(&self) -> ResetCredits {
        let credits = self
            .grants
            .iter()
            .map(|g| {
                let status = if g.paused {
                    "paused"
                } else if g.resets_left == 0 {
                    "spent"
                } else if g.usable_now {
                    "available"
                } else {
                    "blocked"
                };
                let mut description = format!(
                    "{} of {} left · clears {}",
                    g.resets_left,
                    g.resets_total.unwrap_or(g.resets_left),
                    if g.clears.is_empty() {
                        "(unspecified)".to_string()
                    } else {
                        g.clears.join(", ")
                    }
                );
                if g.use_requires_limit {
                    description.push_str(" · usable once a limit is hit");
                }
                ResetCredit {
                    id: Some(g.id.clone()),
                    reset_type: Some(CLAUDE_RATE_LIMITS.to_string()),
                    status: Some(status.to_string()),
                    granted_at: g.starts_at.clone(),
                    expires_at: Some(g.ends_at.clone()),
                    title: g.label.clone(),
                    description: Some(description),
                }
            })
            .collect();
        ResetCredits {
            available_count: self.available_count(),
            credits,
        }
    }
}

/// One `GET /api/oauth/usage?cedar_ember=1` read: the ordinary usage windows
/// (the same body the poller parses) plus the grant status when the server
/// included it. `resets: None` = the `cedar_ember` key was absent or `null`
/// (observed while rate-limited) — unknown, not "no grants".
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeUsageRead {
    pub usage: UsageSnapshot,
    pub resets: Option<ResetGrantStatus>,
}

/// Parse the `cedar_ember` object out of a usage body. `None` for an absent
/// or `null` key, or a shape that is not an object.
pub fn parse_grant_status(body: &Value) -> Option<ResetGrantStatus> {
    let ce = body.get("cedar_ember")?;
    let obj = ce.as_object()?;
    let grants = obj
        .get("grants")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter_map(parse_grant).collect())
        .unwrap_or_default();
    Some(ResetGrantStatus {
        eligible: obj.get("eligible").and_then(Value::as_bool),
        ineligible_reason: string(obj.get("ineligible_reason")),
        at_limit: obj
            .get("at_limit")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        grants,
        next_grant_id: string(obj.get("next_grant_id")),
        weekly_resets_at: string(obj.get("weekly_resets_at")),
        cooldown_until: string(obj.get("cooldown_until")),
    })
}

fn string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// One grant row; `None` when `id`, `resets_left` or `ends_at` is missing or
/// malformed (the web client drops such rows too).
fn parse_grant(row: &Value) -> Option<ResetGrant> {
    let id = string(row.get("id"))?;
    let resets_left = row.get("resets_left").and_then(Value::as_u64)?;
    let ends_at = string(row.get("ends_at"))?;
    Some(ResetGrant {
        id,
        label: string(row.get("label")),
        resets_total: row.get("resets_total").and_then(Value::as_u64),
        resets_left,
        starts_at: string(row.get("starts_at")),
        ends_at,
        clears: strings(row.get("clears")),
        paused: row.get("paused").and_then(Value::as_bool).unwrap_or(false),
        usable_now: row
            .get("usable_now")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        use_requires_limit: row
            .get("use_requires_limit")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

/// Parse a full usage body into windows + grant status. The windows parse
/// is the poller's own ([`parse_usage_body`]), so a body that would fail the
/// poller fails here too — a grant card never rides on an unusable read.
pub fn parse_usage_with_resets(body: &[u8]) -> Result<ClaudeUsageRead, CodexUsageError> {
    let usage = parse_usage_body(body).map_err(|_| CodexUsageError::Malformed)?;
    let value: Value = serde_json::from_slice(body).map_err(|_| CodexUsageError::Malformed)?;
    Ok(ClaudeUsageRead {
        usage,
        resets: parse_grant_status(&value),
    })
}

/// Parse `POST …/reset_rate_limits`. Result codes per the claude.ai bundle
/// (`_A = ["reset","already_used","not_limited","cooldown","ineligible",
/// "unavailable"]`): `reset`/`already_used`/`not_limited` map onto the shared
/// codex outcomes with the same meaning; `ineligible` is a refusal that spent
/// nothing (→ `NoCredit`); `cooldown` is its own terminal-not-spent outcome;
/// `unavailable` — which the web client itself retries with the SAME
/// `request_id` — and anything unknown are UNCERTAIN.
pub fn parse_consume(body: &[u8]) -> Result<ConsumeResult, CodexUsageError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| CodexUsageError::Malformed)?;
    let result = value
        .get("result")
        .and_then(Value::as_str)
        .ok_or(CodexUsageError::Malformed)?;
    let outcome = match result {
        "reset" => ResetOutcome::Reset,
        "already_used" => ResetOutcome::AlreadyRedeemed,
        "not_limited" => ResetOutcome::NothingToReset,
        "ineligible" => ResetOutcome::NoCredit,
        "cooldown" => ResetOutcome::Cooldown,
        _ => return Err(CodexUsageError::UnknownOutcome),
    };
    Ok(ConsumeResult {
        outcome,
        windows_reset: value
            .get("cleared")
            .and_then(Value::as_array)
            .map(|c| c.len() as i64)
            .unwrap_or(0),
    })
}

fn transport(err: &reqwest::Error) -> CodexUsageError {
    CodexUsageError::Transport(if err.is_timeout() {
        "upstream request timed out"
    } else if err.is_connect() {
        "could not connect to upstream"
    } else {
        "upstream request failed"
    })
}

async fn read_body(response: reqwest::Response) -> Result<bytes::Bytes, CodexUsageError> {
    let status = response.status();
    if !status.is_success() {
        return Err(CodexUsageError::Status { status });
    }
    response.bytes().await.map_err(|e| transport(&e))
}

/// `GET {base}/api/oauth/usage?cedar_ember=1` — windows plus grants, one
/// request. A pure read.
pub async fn fetch_usage_with_resets(
    client: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    cli_version: &str,
) -> Result<ClaudeUsageRead, CodexUsageError> {
    let url = format!(
        "{}/api/oauth/usage?cedar_ember=1",
        base_url.trim_end_matches('/')
    );
    let response = client
        .get(url)
        .bearer_auth(access_token)
        .header(http::header::ACCEPT, "application/json")
        .header(http::header::USER_AGENT, claude_cli_user_agent(cli_version))
        .timeout(CONTROL_TIMEOUT)
        .send()
        .await
        .map_err(|e| transport(&e))?;
    parse_usage_with_resets(&read_body(response).await?)
}

/// `POST {base}/api/organizations/{org_uuid}/reset_rate_limits` — THE
/// irreversible call. `request_id` is the caller's idempotency key (this
/// layer never mints one); `grant_id` is mandatory (`grant_id_required` is a
/// server refusal reason).
pub async fn consume_reset_grant(
    client: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    cli_version: &str,
    org_uuid: &str,
    grant_id: &str,
    request_id: &str,
) -> Result<ConsumeResult, CodexUsageError> {
    let url = format!(
        "{}/api/organizations/{org_uuid}/reset_rate_limits",
        base_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "program": PROGRAM,
        "grant_id": grant_id,
        "request_id": request_id,
    });
    let response = client
        .post(url)
        .bearer_auth(access_token)
        .header(http::header::ACCEPT, "application/json")
        .header(http::header::CONTENT_TYPE, "application/json")
        .header(http::header::USER_AGENT, claude_cli_user_agent(cli_version))
        .timeout(CONTROL_TIMEOUT)
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| transport(&e))?;
    parse_consume(&read_body(response).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim `cedar_ember` object from the 2026-10-06 live read through
    /// `api.anthropic.com/api/oauth/usage?cedar_ember=1` (Claude Code
    /// identity; account and org identifiers removed), spliced into the
    /// poller's usage fixture.
    fn live_body(cedar_ember: &str) -> Vec<u8> {
        format!(
            r#"{{
 "five_hour":  {{ "utilization": 2.0,  "resets_at": "2026-10-06T03:59:59.845653+00:00" }},
 "seven_day":  {{ "utilization": 0.0, "resets_at": "2026-10-10T08:59:59.845676+00:00" }},
 "limits": [],
 "cedar_ember": {cedar_ember}
}}"#
        )
        .into_bytes()
    }

    const ELIGIBLE: &str = r#"{
  "eligible": true, "ineligible_reason": null, "at_limit": false, "exhausted": [],
  "grants": [{
    "id": "opus55-launch-promax-20260921",
    "label": "Claude Opus 5.5 launch: one usage-limit reset for Pro and Max",
    "resets_total": 1, "resets_left": 1,
    "starts_at": "2026-09-22T16:00:00+00:00", "ends_at": "2026-10-22T16:00:00+00:00",
    "clears": ["five_hour", "seven_day", "seven_day_overage_included"],
    "paused": false, "usable_now": true, "use_requires_limit": false,
    "percent_used": {"five_hour": 2, "seven_day": 0}, "blocking": [], "arm": null
  }],
  "next_grant_id": "opus55-launch-promax-20260921",
  "weekly_resets_at": "2026-10-12T22:00:00+00:00", "cooldown_until": null,
  "event_props": {"surface": "claude_ai", "tier": "claude_max_20x"}
}"#;

    const SURFACE_INELIGIBLE: &str = r#"{"eligible":false,"ineligible_reason":"surface","at_limit":false,"exhausted":[],"grants":[],"next_grant_id":null,"weekly_resets_at":null,"cooldown_until":null,"event_props":null}"#;

    #[test]
    fn user_agent_is_the_claude_code_identity() {
        assert_eq!(
            claude_cli_user_agent(" 2.1.291 "),
            "claude-cli/2.1.291 (external, cli)"
        );
    }

    #[test]
    fn live_eligible_body_maps_to_one_available_credit_with_expiry() {
        let read = parse_usage_with_resets(&live_body(ELIGIBLE)).expect("parses");
        assert!(read.usage.five_hour.is_some(), "windows still parse");
        let status = read.resets.expect("cedar_ember present");
        assert_eq!(status.eligible, Some(true));
        assert_eq!(status.available_count(), Some(1));
        assert_eq!(status.applicable_count(), Some(1));
        assert_eq!(
            status.weekly_resets_at.as_deref(),
            Some("2026-10-12T22:00:00+00:00")
        );
        let credits = status.to_credits();
        assert_eq!(credits.available_count, Some(1));
        assert_eq!(credits.credits.len(), 1);
        let c = &credits.credits[0];
        assert_eq!(c.id.as_deref(), Some("opus55-launch-promax-20260921"));
        assert_eq!(c.reset_type.as_deref(), Some(CLAUDE_RATE_LIMITS));
        assert_eq!(c.status.as_deref(), Some("available"));
        assert_eq!(c.expires_at.as_deref(), Some("2026-10-22T16:00:00+00:00"));
        assert_eq!(c.granted_at.as_deref(), Some("2026-09-22T16:00:00+00:00"));
        assert!(c.is_redeemable(), "a live usable_now grant is redeemable");
        assert!(c.description.as_deref().unwrap().contains("1 of 1 left"));
    }

    #[test]
    fn surface_ineligible_is_unknown_not_zero() {
        let read = parse_usage_with_resets(&live_body(SURFACE_INELIGIBLE)).expect("parses");
        let status = read.resets.expect("present");
        assert_eq!(status.eligible, Some(false));
        assert_eq!(status.ineligible_reason.as_deref(), Some("surface"));
        assert_eq!(status.available_count(), None, "ineligible ≠ zero");
        assert_eq!(status.applicable_count(), None);
        assert!(status.to_credits().credits.is_empty());
        let e = status.eligibility();
        assert_eq!(e.eligible, Some(false));
        assert_eq!(e.ineligible_reason.as_deref(), Some("surface"));
    }

    #[test]
    fn null_cedar_ember_is_absent_status_not_an_error() {
        // Observed while rate-limited: the key is present but null.
        let read = parse_usage_with_resets(&live_body("null")).expect("parses");
        assert_eq!(read.resets, None);
        let read = parse_usage_with_resets(
            br#"{"five_hour":{"utilization":1.0,"resets_at":"2026-10-06T03:59:59+00:00"}}"#,
        )
        .expect("parses");
        assert_eq!(read.resets, None);
    }

    #[test]
    fn grant_rows_missing_load_bearing_fields_are_dropped() {
        let body = live_body(
            r#"{"eligible":true,"grants":[
              {"id":"g-ok","resets_left":2,"resets_total":3,"ends_at":"2026-11-01T00:00:00Z","usable_now":false,"paused":false},
              {"id":"g-paused","resets_left":1,"ends_at":"2026-11-01T00:00:00Z","usable_now":true,"paused":true},
              {"id":"g-spent","resets_left":0,"ends_at":"2026-11-01T00:00:00Z","usable_now":false},
              {"resets_left":1,"ends_at":"2026-11-01T00:00:00Z"},
              {"id":"g-no-end","resets_left":1}
            ]}"#,
        );
        let status = parse_usage_with_resets(&body)
            .expect("parses")
            .resets
            .expect("present");
        let ids: Vec<&str> = status.grants.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["g-ok", "g-paused", "g-spent"]);
        // Paused and spent grants do not count; the blocked one does.
        assert_eq!(status.available_count(), Some(2));
        assert_eq!(status.applicable_count(), Some(0));
        let credits = status.to_credits();
        let by_id = |id: &str| {
            credits
                .credits
                .iter()
                .find(|c| c.id.as_deref() == Some(id))
                .unwrap()
                .status
                .clone()
                .unwrap()
        };
        assert_eq!(by_id("g-ok"), "blocked");
        assert_eq!(by_id("g-paused"), "paused");
        assert_eq!(by_id("g-spent"), "spent");
        assert!(credits.credits.iter().all(|c| !c.is_redeemable()));
    }

    #[test]
    fn unusable_usage_body_fails_even_with_grants() {
        // A body the poller would reject must not become a grant card.
        assert!(matches!(
            parse_usage_with_resets(b"<html>503</html>"),
            Err(CodexUsageError::Malformed)
        ));
    }

    #[test]
    fn consume_results_map_and_unknown_is_uncertain() {
        for (code, outcome) in [
            ("reset", ResetOutcome::Reset),
            ("already_used", ResetOutcome::AlreadyRedeemed),
            ("not_limited", ResetOutcome::NothingToReset),
            ("ineligible", ResetOutcome::NoCredit),
            ("cooldown", ResetOutcome::Cooldown),
        ] {
            let body = format!(
                r#"{{"result":"{code}","reason":null,"reset":true,"grant_id":"g","resets_left":0,"cleared":["five_hour","seven_day"],"weekly_resets_at":null,"cooldown_until":null}}"#
            );
            let parsed = parse_consume(body.as_bytes()).expect("parses");
            assert_eq!(parsed.outcome, outcome, "{code}");
            assert_eq!(parsed.windows_reset, 2);
        }
        // `unavailable` is the web client's own retry-same-id case.
        assert!(matches!(
            parse_consume(br#"{"result":"unavailable"}"#),
            Err(CodexUsageError::UnknownOutcome)
        ));
        assert!(matches!(
            parse_consume(br#"{"result":"rate_limited_or_new"}"#),
            Err(CodexUsageError::UnknownOutcome)
        ));
        assert!(matches!(
            parse_consume(br#"{"error":"x"}"#),
            Err(CodexUsageError::Malformed)
        ));
    }
}
