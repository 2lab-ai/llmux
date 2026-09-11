//! Codex usage controls — the WHAM wire layer (`.prd/16-codex-usage-controls.md`).
//!
//! Pure HTTP + parsing for the three backend operations the operator drives:
//! read usage, list rate-limit reset credits, redeem exactly one. Primary
//! source: openai/codex @ `fc948f8c473e5d11e780ffcf1fd7f812a2020932`
//! (`codex-rs/backend-client/src/client/rate_limit_resets.rs` + `types.rs`).
//!
//! Two rules this module exists to enforce:
//!
//! - **Unknown is never zero.** A missing / malformed field degrades to
//!   `None`; an HTML error body is a parse error, not "no resets available".
//! - **Nothing leaks.** Errors carry a fixed sanitized phrase — never the
//!   response body, the URL's credentials, or the bearer token.

use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::scheduler::headers::WindowReading;
use crate::scheduler::usage::UsageSnapshot;

/// Per-call ceiling on every usage-control request (contract: bounded
/// timeouts). Deliberately short — these are interactive operator actions.
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(15);

/// Windows at or above this duration are the WEEKLY gauge, below it the
/// session (5h) gauge. Live shapes are 18000 and 604800; the split is by
/// DURATION because `primary_window` carries either one (live capture
/// 2026-09-11 had a weekly primary and a null secondary).
const WEEKLY_MIN_SECS: u64 = 86_400;

#[derive(Debug, Clone, thiserror::Error)]
pub enum CodexUsageError {
    /// The configured codex upstream is not a shape we can derive the WHAM
    /// base from. Never fall back to production — refuse instead.
    #[error("codex upstream is not a usage-control endpoint ({0})")]
    UnsupportedUpstream(&'static str),
    /// Transport failure, reduced to a CLASS (never the source's Display,
    /// which can carry the URL).
    #[error("{0}")]
    Transport(&'static str),
    #[error("upstream returned HTTP {status}")]
    Status { status: http::StatusCode },
    /// Body was not JSON (e.g. an HTML error page) or not the expected shape.
    /// Deliberately does NOT parse into an empty-but-valid state.
    #[error("upstream response was not a valid usage document")]
    Malformed,
    /// A redemption code this build does not know: the redemption outcome is
    /// UNCERTAIN (it may have spent a credit), never a clean failure.
    #[error("upstream reported an unrecognized redemption outcome")]
    UnknownOutcome,
}

impl CodexUsageError {
    /// Sanitized, operator-facing text. Same as `Display` — spelled out so
    /// call sites can be read as "this is the only thing that reaches a log
    /// or an HTTP body".
    pub fn sanitized(&self) -> String {
        self.to_string()
    }
}

/// Derive the WHAM base from the configured codex upstream: same origin, same
/// path prefix, with the required trailing `/codex` segment replaced by
/// `/wham` (`https://chatgpt.com/backend-api/codex` →
/// `https://chatgpt.com/backend-api/wham`).
///
/// Refuses (rather than guesses) anything else: a non-HTTP scheme, userinfo in
/// the authority, a query or fragment, an empty authority, or a path that does
/// not end in `/codex`. A mock upstream therefore stays a mock — this never
/// substitutes the production default.
pub fn wham_base(upstream: &str) -> Result<String, CodexUsageError> {
    let raw = upstream.trim();
    if raw.contains('?') || raw.contains('#') {
        return Err(CodexUsageError::UnsupportedUpstream(
            "query or fragment present",
        ));
    }
    let lower = raw.to_ascii_lowercase();
    let scheme_len = if lower.starts_with("https://") {
        8
    } else if lower.starts_with("http://") {
        7
    } else {
        return Err(CodexUsageError::UnsupportedUpstream("not an http(s) url"));
    };
    let (scheme, rest) = raw.split_at(scheme_len);
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return Err(CodexUsageError::UnsupportedUpstream("empty authority"));
    }
    if authority.contains('@') {
        return Err(CodexUsageError::UnsupportedUpstream(
            "credentials in the url",
        ));
    }
    let Some(prefix) = path.trim_end_matches('/').strip_suffix("/codex") else {
        return Err(CodexUsageError::UnsupportedUpstream(
            "path does not end in /codex",
        ));
    };
    Ok(format!("{scheme}{authority}{prefix}/wham"))
}

/// One `GET /wham/usage` observation: the account windows plus the optional
/// reset-credit counters that ride the same body. `None` counts mean UNKNOWN
/// (the field was absent or unusable) — never "zero left".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CodexUsage {
    pub usage: UsageSnapshot,
    pub available_resets: Option<u64>,
    pub applicable_resets: Option<u64>,
}

/// `GET /wham/rate-limit-reset-credits`. `available_count` is `None` when the
/// field is missing/invalid — unknown, not zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResetCredits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_count: Option<u64>,
    #[serde(default)]
    pub credits: Vec<ResetCredit>,
}

/// One entitlement row, parsed DEFENSIVELY: every field is optional here even
/// though the codex client's own type requires `id`
/// (`codex-rs/backend-client/src/types.rs`, pinned `fc948f8c`). That is
/// tolerance, not an observed schema — the local capture in
/// `.prd/16-codex-usage-controls.md` had its `id`/`profile_*` fields stripped
/// by the sanitizer, so it is NOT evidence that upstream omits them. Upstream
/// also ships fields (`total_earned_count`, `profile_*`, `redeem_*`) this
/// integration deliberately ignores.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResetCredit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The reset credit type llmux redeems. Other `reset_type`s (should they
/// appear) are not assumed redeemable here.
pub const CODEX_RATE_LIMITS: &str = "codex_rate_limits";

impl ResetCredit {
    /// A row that is redeemable RIGHT NOW per the upstream list: status
    /// `available` and the codex rate-limit reset type.
    pub fn is_redeemable(&self) -> bool {
        self.status.as_deref() == Some("available")
            && self.reset_type.as_deref() == Some(CODEX_RATE_LIMITS)
    }
}

/// The four terminal codes of `POST …/consume`. Anything else is
/// [`CodexUsageError::UnknownOutcome`] — uncertain, not a fifth outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetOutcome {
    Reset,
    AlreadyRedeemed,
    NothingToReset,
    NoCredit,
}

impl ResetOutcome {
    /// Did upstream actually touch the windows? `reset`/`already_redeemed`
    /// mean the entitlement is spent and a re-read is warranted.
    pub fn spent(self) -> bool {
        matches!(self, Self::Reset | Self::AlreadyRedeemed)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Reset => "reset",
            Self::AlreadyRedeemed => "already_redeemed",
            Self::NothingToReset => "nothing_to_reset",
            Self::NoCredit => "no_credit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumeResult {
    pub outcome: ResetOutcome,
    pub windows_reset: i64,
}

/// Parse `GET /wham/usage`. `now` only feeds the `reset_after_seconds`
/// fallback when no absolute `reset_at` is present.
///
/// STRUCTURE vs CONTENT. A usage document must carry a `rate_limit` OBJECT
/// holding at least one of the two window keys; anything else (`{}`,
/// `{"rate_limit":{}}`, an unrelated JSON object, an HTML error page) is
/// [`CodexUsageError::Malformed`] rather than an empty success — an unusable
/// body must not be able to advance a "last successful refresh" timestamp or
/// pass for fresh quota.
///
/// Within that structure a `null` window is a VALID absent window — the live
/// capture's `secondary_window` is exactly that — and is skipped without
/// complaint. A window that is PRESENT but unreadable (wrong types, missing
/// duration, unusable reset) fails the whole read instead of silently
/// contributing nothing.
pub fn parse_usage(body: &[u8], now: SystemTime) -> Result<CodexUsage, CodexUsageError> {
    const WINDOWS: [&str; 2] = ["primary_window", "secondary_window"];
    let value: Value = serde_json::from_slice(body).map_err(|_| CodexUsageError::Malformed)?;
    let limit = value
        .get("rate_limit")
        .and_then(Value::as_object)
        .ok_or(CodexUsageError::Malformed)?;
    if !WINDOWS.iter().any(|key| limit.contains_key(*key)) {
        return Err(CodexUsageError::Malformed);
    }
    let mut usage = UsageSnapshot::default();
    for key in WINDOWS {
        let Some(row) = limit.get(key) else { continue };
        if row.is_null() {
            continue; // legitimately absent, not malformed
        }
        // Window KIND comes from `limit_window_seconds`, never from the
        // primary/secondary position: the live weekly reading arrived as the
        // primary window with a null secondary.
        let (weekly, reading) = window(row, now).ok_or(CodexUsageError::Malformed)?;
        let slot = if weekly {
            &mut usage.seven_day
        } else {
            &mut usage.five_hour
        };
        *slot = Some(reading);
    }
    let credits = value.get("rate_limit_reset_credits");
    Ok(CodexUsage {
        usage,
        available_resets: count(credits, "available_count"),
        applicable_resets: count(credits, "applicable_available_count"),
    })
}

/// One PRESENT window row → `(is_weekly, reading)`, or `None` when a
/// load-bearing field is missing or unusable — which the caller turns into a
/// failed read, never into a fabricated 0%.
fn window(value: &Value, now: SystemTime) -> Option<(bool, WindowReading)> {
    let percent = value.get("used_percent")?.as_f64()?;
    if !percent.is_finite() || percent < 0.0 {
        return None;
    }
    let duration = value.get("limit_window_seconds")?.as_u64()?;
    if duration == 0 {
        return None;
    }
    let resets_at = match value.get("reset_at").and_then(Value::as_u64) {
        Some(epoch) if epoch > 0 => SystemTime::UNIX_EPOCH + Duration::from_secs(epoch),
        _ => {
            let after = value.get("reset_after_seconds")?.as_u64()?;
            now.checked_add(Duration::from_secs(after))?
        }
    };
    Some((
        duration >= WEEKLY_MIN_SECS,
        WindowReading {
            utilization: (percent / 100.0).clamp(0.0, 1.0),
            resets_at,
        },
    ))
}

/// A non-negative integer counter, or `None` (UNKNOWN) when absent/invalid —
/// notably never `Some(0)` by default.
fn count(value: Option<&Value>, key: &str) -> Option<u64> {
    value?.get(key)?.as_u64()
}

/// Parse `GET /wham/rate-limit-reset-credits`. Unknown rows degrade field by
/// field; a non-JSON body is [`CodexUsageError::Malformed`].
pub fn parse_credits(body: &[u8]) -> Result<ResetCredits, CodexUsageError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| CodexUsageError::Malformed)?;
    if !value.is_object() {
        return Err(CodexUsageError::Malformed);
    }
    let credits = value
        .get("credits")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| serde_json::from_value::<ResetCredit>(row.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    Ok(ResetCredits {
        available_count: count(Some(&value), "available_count"),
        credits,
    })
}

/// Parse `POST …/consume`. An unrecognized `code` is UNCERTAIN.
pub fn parse_consume(body: &[u8]) -> Result<ConsumeResult, CodexUsageError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| CodexUsageError::Malformed)?;
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .ok_or(CodexUsageError::Malformed)?;
    let outcome = match code {
        "reset" => ResetOutcome::Reset,
        "already_redeemed" => ResetOutcome::AlreadyRedeemed,
        "nothing_to_reset" => ResetOutcome::NothingToReset,
        "no_credit" => ResetOutcome::NoCredit,
        _ => return Err(CodexUsageError::UnknownOutcome),
    };
    Ok(ConsumeResult {
        outcome,
        windows_reset: value
            .get("windows_reset")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    })
}

/// Transport failures reduced to a fixed phrase per class: `reqwest::Error`'s
/// own `Display` can carry the request URL, so it never reaches a caller.
fn transport(err: &reqwest::Error) -> CodexUsageError {
    CodexUsageError::Transport(if err.is_timeout() {
        "upstream request timed out"
    } else if err.is_connect() {
        "could not connect to the upstream"
    } else if err.is_body() || err.is_decode() {
        "upstream response could not be read"
    } else {
        "upstream request failed"
    })
}

/// The auth pair every WHAM call carries (codex client parity): the account's
/// own bearer token and its ChatGPT account id.
fn authed(
    builder: reqwest::RequestBuilder,
    access_token: &str,
    account_id: &str,
) -> reqwest::RequestBuilder {
    builder
        .bearer_auth(access_token)
        .header("ChatGPT-Account-Id", account_id)
        .header(http::header::ACCEPT, "application/json")
        .timeout(CONTROL_TIMEOUT)
}

async fn read_body(response: reqwest::Response) -> Result<bytes::Bytes, CodexUsageError> {
    let status = response.status();
    if !status.is_success() {
        return Err(CodexUsageError::Status { status });
    }
    response.bytes().await.map_err(|e| transport(&e))
}

/// `GET {base}/usage` for one account.
pub async fn fetch_usage(
    client: &reqwest::Client,
    base: &str,
    access_token: &str,
    account_id: &str,
    now: SystemTime,
) -> Result<CodexUsage, CodexUsageError> {
    let response = authed(
        client.get(format!("{base}/usage")),
        access_token,
        account_id,
    )
    .send()
    .await
    .map_err(|e| transport(&e))?;
    parse_usage(&read_body(response).await?, now)
}

/// `GET {base}/rate-limit-reset-credits` — a pure read, never a mutation.
pub async fn fetch_reset_credits(
    client: &reqwest::Client,
    base: &str,
    access_token: &str,
    account_id: &str,
) -> Result<ResetCredits, CodexUsageError> {
    let response = authed(
        client.get(format!("{base}/rate-limit-reset-credits")),
        access_token,
        account_id,
    )
    .send()
    .await
    .map_err(|e| transport(&e))?;
    parse_credits(&read_body(response).await?)
}

/// `POST {base}/rate-limit-reset-credits/consume` — THE irreversible call.
/// `redeem_request_id` is the caller's idempotency key (this layer never mints
/// one) and `credit_id` is omitted from the body entirely when `None`, per the
/// codex client (an explicit null is a different request).
pub async fn consume_reset_credit(
    client: &reqwest::Client,
    base: &str,
    access_token: &str,
    account_id: &str,
    redeem_request_id: &str,
    credit_id: Option<&str>,
) -> Result<ConsumeResult, CodexUsageError> {
    let mut body = serde_json::Map::new();
    body.insert("redeem_request_id".into(), redeem_request_id.into());
    if let Some(credit_id) = credit_id {
        body.insert("credit_id".into(), credit_id.into());
    }
    let response = authed(
        client
            .post(format!("{base}/rate-limit-reset-credits/consume"))
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Value::Object(body).to_string()),
        access_token,
        account_id,
    )
    .send()
    .await
    .map_err(|e| transport(&e))?;
    parse_consume(&read_body(response).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn wham_base_replaces_the_trailing_codex_segment() {
        assert_eq!(
            wham_base("https://chatgpt.com/backend-api/codex").unwrap(),
            "https://chatgpt.com/backend-api/wham"
        );
        assert_eq!(
            wham_base("http://127.0.0.1:8080/backend-api/codex/").unwrap(),
            "http://127.0.0.1:8080/backend-api/wham",
            "trailing slashes are trimmed"
        );
        assert_eq!(
            wham_base("https://example.test/codex").unwrap(),
            "https://example.test/wham"
        );
    }

    #[test]
    fn wham_base_refuses_shapes_it_cannot_derive() {
        for bad in [
            "https://chatgpt.com/backend-api", // no /codex segment
            "https://codex",                   // authority, not a path segment
            "ftp://chatgpt.com/backend-api/codex",
            "https://user:pw@chatgpt.com/backend-api/codex",
            "https://chatgpt.com/backend-api/codex?x=1",
            "https:///backend-api/codex",
            "",
        ] {
            assert!(
                wham_base(bad).is_err(),
                "{bad:?} must be refused, never defaulted to production"
            );
        }
    }

    #[test]
    fn live_usage_maps_the_weekly_primary_window_to_seven_day() {
        // Live capture 2026-09-11: primary_window IS the weekly gauge
        // (604800s) and secondary is null.
        let body = br#"{
          "rate_limit": {
            "primary_window": {"used_percent": 43, "limit_window_seconds": 604800,
                               "reset_after_seconds": 349683, "reset_at": 1789442251},
            "secondary_window": null
          },
          "rate_limit_reset_credits": {"available_count": 3, "applicable_available_count": 0}
        }"#;
        let parsed = parse_usage(body, at(1_000_000)).unwrap();
        let seven = parsed.usage.seven_day.expect("weekly window");
        assert!((seven.utilization - 0.43).abs() < 1e-9);
        assert_eq!(seven.resets_at, at(1_789_442_251));
        assert!(
            parsed.usage.five_hour.is_none(),
            "a weekly window is never filed as the 5h window"
        );
        assert_eq!(parsed.available_resets, Some(3));
        assert_eq!(
            parsed.applicable_resets,
            Some(0),
            "applicable is its own counter, not a copy of available"
        );
    }

    #[test]
    fn five_hour_duration_lands_on_the_session_window() {
        let body = br#"{"rate_limit":{"primary_window":
            {"used_percent":12,"limit_window_seconds":18000,"reset_at":1789442251}}}"#;
        let parsed = parse_usage(body, at(0)).unwrap();
        assert!((parsed.usage.five_hour.unwrap().utilization - 0.12).abs() < 1e-9);
        assert!(parsed.usage.seven_day.is_none());
    }

    #[test]
    fn present_but_unreadable_windows_fail_instead_of_becoming_zeros() {
        // A window OBJECT that cannot be read is a STRUCTURAL failure, not an
        // empty success: contributing nothing silently would let a bad body
        // pass for a fresh observation and advance the refresh timestamp.
        for body in [
            br#"{"rate_limit":{"primary_window":{"used_percent":"high","limit_window_seconds":604800}}}"#.as_slice(),
            br#"{"rate_limit":{"secondary_window":{"used_percent":5}}}"#.as_slice(),
            br#"{"rate_limit":{"primary_window":{"used_percent":-1,"limit_window_seconds":604800,"reset_at":1}}}"#.as_slice(),
            br#"{"rate_limit":{"primary_window":{"used_percent":5,"limit_window_seconds":0,"reset_at":1}}}"#.as_slice(),
        ] {
            assert!(
                matches!(parse_usage(body, at(0)), Err(CodexUsageError::Malformed)),
                "an unreadable window must fail the read: {}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn null_windows_are_valid_and_absent_not_malformed() {
        // Do NOT over-correct: the live shape carries an explicitly null
        // secondary window and must keep parsing.
        let parsed = parse_usage(
            br#"{"rate_limit":{"primary_window":null,"secondary_window":null}}"#,
            at(0),
        )
        .unwrap();
        assert_eq!(parsed.usage, UsageSnapshot::default(), "absent, not zero");
        assert_eq!(parsed.available_resets, None, "unknown count stays unknown");
        assert_eq!(parsed.applicable_resets, None);
    }

    #[test]
    fn a_body_that_is_not_a_usage_document_is_malformed() {
        // `{}` and `{"rate_limit":{}}` used to parse as an empty SUCCESS,
        // which would advance the last-successful-refresh timestamp on a body
        // carrying no quota information whatsoever.
        for body in [
            b"{}".as_slice(),
            br#"{"rate_limit":{}}"#.as_slice(),
            br#"{"rate_limit":"nope"}"#.as_slice(),
            br#"{"rate_limit":null}"#.as_slice(),
            br#"{"detail":"something else entirely"}"#.as_slice(),
        ] {
            assert!(
                matches!(parse_usage(body, at(0)), Err(CodexUsageError::Malformed)),
                "{} must not parse as an empty success",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn html_error_body_is_malformed_not_empty_state() {
        assert!(matches!(
            parse_usage(b"<html>502</html>", at(0)),
            Err(CodexUsageError::Malformed)
        ));
        assert!(matches!(
            parse_credits(b"<html>502</html>"),
            Err(CodexUsageError::Malformed)
        ));
    }

    #[test]
    fn reset_after_seconds_is_the_fallback_when_no_absolute_reset() {
        let body = br#"{"rate_limit":{"primary_window":
            {"used_percent":50,"limit_window_seconds":18000,"reset_after_seconds":600}}}"#;
        let parsed = parse_usage(body, at(1_000)).unwrap();
        assert_eq!(parsed.usage.five_hour.unwrap().resets_at, at(1_600));
    }

    #[test]
    fn credits_parse_tolerates_missing_ids_and_extra_fields() {
        // Defensive tolerance: a row missing `id` still parses (the field is
        // required by the codex client's type, so this is robustness, not an
        // observed shape), and unknown fields are ignored.
        let body = br#"{"available_count":3,"total_earned_count":9,"credits":[
            {"reset_type":"codex_rate_limits","status":"available",
             "granted_at":"2026-08-22T00:00:34Z","expires_at":"2026-09-21T00:00:34Z",
             "title":"Full reset","description":"granted","profile_user_id":"@x"}]}"#;
        let parsed = parse_credits(body).unwrap();
        assert_eq!(parsed.available_count, Some(3));
        assert_eq!(parsed.credits.len(), 1);
        assert_eq!(parsed.credits[0].id, None, "missing id stays unknown");
        assert!(parsed.credits[0].is_redeemable());

        let unknown = br#"{"credits":[{"reset_type":"other","status":"redeemed"}]}"#;
        let parsed = parse_credits(unknown).unwrap();
        assert_eq!(parsed.available_count, None, "absent count is unknown");
        assert!(!parsed.credits[0].is_redeemable());
    }

    #[test]
    fn all_four_consume_codes_parse_and_a_fifth_is_uncertain() {
        for (code, expected) in [
            ("reset", ResetOutcome::Reset),
            ("already_redeemed", ResetOutcome::AlreadyRedeemed),
            ("nothing_to_reset", ResetOutcome::NothingToReset),
            ("no_credit", ResetOutcome::NoCredit),
        ] {
            let body = format!(r#"{{"code":"{code}","windows_reset":2}}"#);
            let parsed = parse_consume(body.as_bytes()).unwrap();
            assert_eq!(parsed.outcome, expected);
            assert_eq!(parsed.windows_reset, 2);
        }
        assert!(matches!(
            parse_consume(br#"{"code":"teleported"}"#),
            Err(CodexUsageError::UnknownOutcome)
        ));
        assert!(matches!(
            parse_consume(br#"{"windows_reset":1}"#),
            Err(CodexUsageError::Malformed)
        ));
        assert!(
            ResetOutcome::Reset.spent() && ResetOutcome::AlreadyRedeemed.spent(),
            "both spend the entitlement → re-read"
        );
        assert!(!ResetOutcome::NothingToReset.spent() && !ResetOutcome::NoCredit.spent());
    }
}
