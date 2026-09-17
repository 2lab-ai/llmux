//! Grok usage source — the xAI CLI **billing** endpoint
//! (`GET {grok upstream}/billing?format=credits`).
//!
//! Source of truth for the wire contract: xai-org/grok-build
//! `crates/codegen/xai-grok-shell/src/extensions/billing.rs` (+ `credit_bar.rs`
//! for the display semantics); the same strings live in the local grok CLI
//! 1.0.34 binary. Live capture 2026-09-17 (HTTP 200):
//!
//! ```json
//! {"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY",
//!   "start":"2026-09-15T02:19:18.817992+00:00",
//!   "end":"2026-09-22T02:19:18.817992+00:00"},
//!   "creditUsagePercent":65.0,"onDemandCap":{"val":0},"onDemandUsed":{"val":0},
//!   "productUsage":[{"product":"GrokBuild","usagePercent":65.0},{"product":"GrokChat"}],
//!   "isUnifiedBillingUser":true,"prepaidBalance":{"val":0},
//!   "topUpMethod":"TOP_UP_METHOD_SAVED_PAYMENT_METHOD",
//!   "billingPeriodStart":"2026-09-15T02:19:18.817992+00:00",
//!   "billingPeriodEnd":"2026-09-22T02:19:18.817992+00:00"}}
//! ```
//!
//! An expired token answers HTTP 401 (`Invalid or expired credentials …`).
//!
//! Same two rules as [`super::codex_usage`], which this module mirrors:
//!
//! - **Unknown is never zero.** No `currentPeriod`, no period `type`, or no
//!   usable reset instant means NO window — not a 0% gauge. The one exception
//!   is upstream's own encoding: proto3 JSON omits zero scalars, so an ABSENT
//!   `creditUsagePercent` next to a PRESENT period really is 0%.
//! - **Nothing leaks.** Errors carry a fixed sanitized phrase — never the
//!   response body, the URL, or the bearer token.

use serde_json::Value;

use super::codex_usage::CONTROL_TIMEOUT;
use crate::provider::grok::{
    GROK_CLIENT_VERSION_HEADER, GROK_CLIENT_VERSION_VALUE, GROK_TOKEN_AUTH_HEADER,
    GROK_TOKEN_AUTH_VALUE,
};
use crate::scheduler::headers::{parse_rfc3339, WindowReading};
use crate::scheduler::usage::{ScopedLimitReading, UsageSnapshot};
use crate::scheduler::window::LimitSeverity;

/// Account identity the billing endpoint expects next to the bearer token
/// (grok-build `billing.rs`): the credential's `subject` (`sub` claim).
const GROK_USER_ID_HEADER: &str = "x-userid";

/// Substring of `currentPeriod.type` that marks the WEEKLY allowance — the
/// window the 7d gauge shows. Matched as a substring because the wire value is
/// the proto enum name (`USAGE_PERIOD_TYPE_WEEKLY`).
const WEEKLY: &str = "WEEKLY";
/// Substring marking the monthly allowance, which gets a scoped gauge instead
/// (the 7d slot must not claim a 30-day window).
const MONTHLY: &str = "MONTHLY";

/// Scope label for a monthly billing period.
const MONTHLY_LABEL: &str = "Monthly limit";

#[derive(Debug, Clone, thiserror::Error)]
pub enum GrokUsageError {
    /// The configured grok upstream is not a shape the billing URL can be
    /// derived from. Never fall back to production — refuse instead.
    #[error("grok upstream is not a billing endpoint ({0})")]
    UnsupportedUpstream(&'static str),
    /// Transport failure, reduced to a CLASS (never the source's Display,
    /// which can carry the URL).
    #[error("{0}")]
    Transport(&'static str),
    #[error("upstream returned HTTP {status}")]
    Status { status: http::StatusCode },
    /// Body was not JSON (e.g. an HTML error page) or not a billing document.
    /// Deliberately does NOT parse into an empty-but-valid state.
    #[error("upstream response was not a valid billing document")]
    Malformed,
}

impl GrokUsageError {
    /// Sanitized, operator-facing text. Same as `Display` — spelled out so
    /// call sites can be read as "this is the only thing that reaches a log
    /// or an HTTP body".
    pub fn sanitized(&self) -> String {
        self.to_string()
    }
}

/// One `GET /billing?format=credits` observation.
///
/// `usage` carries ONLY what the document proves: the weekly period lands on
/// `seven_day`, a monthly (or otherwise-named) period becomes a scoped
/// reading, and an unreadable/absent period leaves both empty. `five_hour` is
/// never set here — grok's burst gauge comes from the response
/// `x-ratelimit-*` headers and must stay untouched.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GrokBilling {
    pub usage: UsageSnapshot,
    /// Raw `currentPeriod.type` (e.g. `USAGE_PERIOD_TYPE_WEEKLY`), or `None`
    /// when the document carried no period type.
    pub period_type: Option<String>,
    /// `creditUsagePercent` as sent: 0..=100 USED. `None` = unknown (no period
    /// at all), `Some(0.0)` = upstream's omitted-zero encoding.
    pub used_percent: Option<f64>,
}

/// Derive the billing URL from the configured grok upstream: same origin and
/// path, with `/billing?format=credits` appended
/// (`https://cli-chat-proxy.grok.com/v1` →
/// `https://cli-chat-proxy.grok.com/v1/billing?format=credits`).
///
/// Refuses (rather than guesses) anything else: a non-HTTP scheme, userinfo in
/// the authority, a query or fragment, an empty authority, or a path that does
/// not end in `/v1`. A mock upstream therefore stays a mock — this never
/// substitutes the production default.
pub fn billing_url(upstream: &str) -> Result<String, GrokUsageError> {
    let raw = upstream.trim();
    if raw.contains('?') || raw.contains('#') {
        return Err(GrokUsageError::UnsupportedUpstream(
            "query or fragment present",
        ));
    }
    let lower = raw.to_ascii_lowercase();
    let scheme_len = if lower.starts_with("https://") {
        8
    } else if lower.starts_with("http://") {
        7
    } else {
        return Err(GrokUsageError::UnsupportedUpstream("not an http(s) url"));
    };
    let (scheme, rest) = raw.split_at(scheme_len);
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return Err(GrokUsageError::UnsupportedUpstream("empty authority"));
    }
    if authority.contains('@') {
        return Err(GrokUsageError::UnsupportedUpstream(
            "credentials in the url",
        ));
    }
    let path = path.trim_end_matches('/');
    if !path.ends_with("/v1") {
        return Err(GrokUsageError::UnsupportedUpstream(
            "path does not end in /v1",
        ));
    }
    Ok(format!("{scheme}{authority}{path}/billing?format=credits"))
}

/// `GET {upstream}/billing?format=credits` for one grok account, with the
/// grok-CLI identity set the endpoint expects (grok-build `billing.rs`).
pub async fn fetch_billing(
    client: &reqwest::Client,
    upstream: &str,
    access_token: &str,
    subject: &str,
) -> Result<GrokBilling, GrokUsageError> {
    let url = billing_url(upstream)?;
    let response = client
        .get(url)
        .bearer_auth(access_token)
        .header(GROK_TOKEN_AUTH_HEADER, GROK_TOKEN_AUTH_VALUE)
        .header(GROK_USER_ID_HEADER, subject)
        .header(GROK_CLIENT_VERSION_HEADER, GROK_CLIENT_VERSION_VALUE)
        .header(http::header::ACCEPT, "application/json")
        .timeout(CONTROL_TIMEOUT)
        .send()
        .await
        .map_err(|e| transport(&e))?;
    let status = response.status();
    if !status.is_success() {
        return Err(GrokUsageError::Status { status });
    }
    let body = response.bytes().await.map_err(|e| transport(&e))?;
    parse_billing(&body)
}

/// Parse the billing document.
///
/// STRUCTURE vs CONTENT. A billing document must carry a `config` OBJECT;
/// anything else (an HTML error page, a 401 error envelope, an unrelated JSON
/// object) is [`GrokUsageError::Malformed`] rather than an empty success — an
/// unusable body must not pass for a fresh quota observation.
///
/// Within that structure the window is derived ONLY from facts present:
/// `currentPeriod.type` decides which gauge (`WEEKLY` → the 7d slot, anything
/// else → a scoped gauge so a monthly allowance cannot masquerade as weekly),
/// and the reset instant is `currentPeriod.end` with `billingPeriodEnd` as the
/// fallback. Missing period, missing type or missing end ⇒ no window at all.
///
/// Takes NO clock: unlike [`super::codex_usage::parse_usage`] (which resolves a
/// relative `reset_after_seconds`), grok's document carries only ABSOLUTE
/// RFC 3339 instants, so there is nothing for a `now` to resolve.
pub fn parse_billing(body: &[u8]) -> Result<GrokBilling, GrokUsageError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| GrokUsageError::Malformed)?;
    let config = value
        .get("config")
        .and_then(Value::as_object)
        .ok_or(GrokUsageError::Malformed)?;
    let period = config.get("currentPeriod").and_then(Value::as_object);
    let period_type = period
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .map(str::to_string);
    // proto3 JSON omits zero scalars, so an ABSENT percent next to a PRESENT
    // period is a real 0% — while a percent that is present but unreadable
    // fails the whole read rather than becoming a fabricated 0.
    let used_percent = match config.get("creditUsagePercent") {
        Some(raw) => {
            let percent = raw.as_f64().ok_or(GrokUsageError::Malformed)?;
            if !percent.is_finite() || percent < 0.0 {
                return Err(GrokUsageError::Malformed);
            }
            Some(percent)
        }
        None if period.is_some() => Some(0.0),
        None => None,
    };
    let resets_at = period
        .and_then(|p| p.get("end"))
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)
        .or_else(|| {
            config
                .get("billingPeriodEnd")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339)
        });

    let mut usage = UsageSnapshot::default();
    if let (Some(period_type), Some(percent), Some(resets_at)) =
        (period_type.as_deref(), used_percent, resets_at)
    {
        let reading = WindowReading {
            utilization: (percent / 100.0).clamp(0.0, 1.0),
            resets_at,
        };
        if period_type.contains(WEEKLY) {
            usage.seven_day = Some(reading);
        } else {
            // A non-weekly allowance gets its own labelled gauge: the 7d slot
            // would misstate the horizon, and dropping the reading would hide
            // a real limit.
            usage.scoped.push(ScopedLimitReading {
                scope_label: scope_label(period_type),
                reading,
                severity: LimitSeverity::Normal,
                is_active: false,
            });
        }
    }
    Ok(GrokBilling {
        usage,
        period_type,
        used_percent,
    })
}

/// Display label for a non-weekly period: the known monthly case gets prose,
/// anything else keeps the raw enum name (extensible upstream — never guessed).
fn scope_label(period_type: &str) -> String {
    if period_type.contains(MONTHLY) {
        MONTHLY_LABEL.to_string()
    } else {
        period_type.to_string()
    }
}

/// Transport failures reduced to a fixed phrase per class: `reqwest::Error`'s
/// own `Display` can carry the request URL, so it never reaches a caller.
fn transport(err: &reqwest::Error) -> GrokUsageError {
    GrokUsageError::Transport(if err.is_timeout() {
        "upstream request timed out"
    } else if err.is_connect() {
        "could not connect to the upstream"
    } else if err.is_body() || err.is_decode() {
        "upstream response could not be read"
    } else {
        "upstream request failed"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 2026-09-17 live capture, VERBATIM (scratchpad
    /// `grok-billing-capture-2026-09-17.txt`).
    const LIVE_BILLING: &str = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-09-15T02:19:18.817992+00:00","end":"2026-09-22T02:19:18.817992+00:00"},"creditUsagePercent":65.0,"onDemandCap":{"val":0},"onDemandUsed":{"val":0},"productUsage":[{"product":"GrokBuild","usagePercent":65.0},{"product":"GrokChat"}],"isUnifiedBillingUser":true,"prepaidBalance":{"val":0},"topUpMethod":"TOP_UP_METHOD_SAVED_PAYMENT_METHOD","billingPeriodStart":"2026-09-15T02:19:18.817992+00:00","billingPeriodEnd":"2026-09-22T02:19:18.817992+00:00"}}"#;

    /// The 2026-09-17 live 401 body, verbatim.
    const EXPIRED_BODY: &str = r#"{"error":"Invalid or expired credentials (auth_kind=bearer, x_xai_token_auth=xai-grok-cli, upstream=PermissionDenied, reason=no auth context)"}"#;

    use std::time::SystemTime;

    fn epoch_of(at: SystemTime) -> u64 {
        at.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs()
    }

    #[test]
    fn billing_url_appends_the_credits_query_to_the_v1_base() {
        assert_eq!(
            billing_url("https://cli-chat-proxy.grok.com/v1").unwrap(),
            "https://cli-chat-proxy.grok.com/v1/billing?format=credits"
        );
        assert_eq!(
            billing_url("http://127.0.0.1:8080/v1/").unwrap(),
            "http://127.0.0.1:8080/v1/billing?format=credits",
            "trailing slashes are trimmed"
        );
        assert_eq!(
            billing_url("https://example.test/api/v1").unwrap(),
            "https://example.test/api/v1/billing?format=credits"
        );
    }

    #[test]
    fn billing_url_refuses_shapes_it_cannot_derive() {
        for bad in [
            "https://api.x.ai/v2",             // not the /v1 base
            "https://cli-chat-proxy.grok.com", // authority only, no /v1 path
            "https://user:pw@cli-chat-proxy.grok.com/v1",
            "https://cli-chat-proxy.grok.com/v1?format=credits",
            "https://cli-chat-proxy.grok.com/v1#frag",
            "ftp://cli-chat-proxy.grok.com/v1",
            "https:///v1",
            "",
        ] {
            assert!(
                billing_url(bad).is_err(),
                "{bad:?} must be refused, never defaulted to production"
            );
        }
    }

    #[test]
    fn live_capture_maps_the_weekly_period_to_seven_day() {
        let parsed = parse_billing(LIVE_BILLING.as_bytes()).unwrap();
        let seven = parsed.usage.seven_day.expect("weekly window");
        assert!((seven.utilization - 0.65).abs() < 1e-9);
        // 2026-09-22T02:19:18Z
        assert_eq!(epoch_of(seven.resets_at), 1_790_043_558);
        assert!(
            parsed.usage.five_hour.is_none(),
            "the billing period never feeds the 5h burst gauge (headers own it)"
        );
        assert!(parsed.usage.scoped.is_empty(), "weekly is not a scoped row");
        assert_eq!(
            parsed.period_type.as_deref(),
            Some("USAGE_PERIOD_TYPE_WEEKLY")
        );
        assert_eq!(parsed.used_percent, Some(65.0));
    }

    #[test]
    fn absent_percent_with_a_period_is_zero_not_unknown() {
        // proto3 JSON omits zero scalars: 0% used arrives as NO field.
        let body = br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY",
            "end":"2026-09-22T02:19:18.817992+00:00"}}}"#;
        let parsed = parse_billing(body).unwrap();
        assert_eq!(parsed.used_percent, Some(0.0));
        assert_eq!(
            parsed.usage.seven_day.expect("weekly window").utilization,
            0.0
        );
    }

    #[test]
    fn monthly_period_becomes_a_scoped_reading_not_the_seven_day_gauge() {
        let body = br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_MONTHLY",
            "end":"2026-10-15T02:19:18.817992+00:00"},"creditUsagePercent":12.5}}"#;
        let parsed = parse_billing(body).unwrap();
        assert!(
            parsed.usage.seven_day.is_none(),
            "a 30-day allowance must not be shown as the weekly gauge"
        );
        assert_eq!(parsed.usage.scoped.len(), 1);
        let scoped = &parsed.usage.scoped[0];
        assert_eq!(scoped.scope_label, "Monthly limit");
        assert!((scoped.reading.utilization - 0.125).abs() < 1e-9);
        assert_eq!(scoped.severity, LimitSeverity::Normal);
        assert!(!scoped.is_active);

        // An unknown period kind keeps its raw enum name rather than guessing.
        let body = br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_DAILY",
            "end":"2026-09-18T02:19:18.817992+00:00"},"creditUsagePercent":40}}"#;
        let parsed = parse_billing(body).unwrap();
        assert_eq!(
            parsed.usage.scoped[0].scope_label,
            "USAGE_PERIOD_TYPE_DAILY"
        );
        assert!(parsed.usage.seven_day.is_none());
    }

    #[test]
    fn missing_period_or_reset_yields_no_window_never_zero() {
        // No currentPeriod at all: the percent is unknown, not 0.
        let parsed = parse_billing(br#"{"config":{"isUnifiedBillingUser":true}}"#).unwrap();
        assert_eq!(parsed.usage, UsageSnapshot::default(), "absent, not zero");
        assert_eq!(parsed.used_percent, None, "unknown stays unknown");
        assert_eq!(parsed.period_type, None);

        // A period with no reset instant anywhere: no window.
        let no_end = br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY",
            "start":"2026-09-15T02:19:18.817992+00:00"},"creditUsagePercent":65}}"#;
        let parsed = parse_billing(no_end).unwrap();
        assert!(parsed.usage.seven_day.is_none(), "no reset ⇒ no gauge");
        assert_eq!(parsed.used_percent, Some(65.0), "the percent is still read");

        // A period with no TYPE: the gauge kind is unknown, so no window.
        let no_type = br#"{"config":{"currentPeriod":{"end":"2026-09-22T02:19:18.817992+00:00"},
            "creditUsagePercent":65}}"#;
        let parsed = parse_billing(no_type).unwrap();
        assert_eq!(parsed.usage, UsageSnapshot::default());
    }

    #[test]
    fn billing_period_end_is_the_reset_fallback() {
        let body = br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY"},
            "creditUsagePercent":65,
            "billingPeriodEnd":"2026-09-22T02:19:18.817992+00:00"}}"#;
        let parsed = parse_billing(body).unwrap();
        assert_eq!(
            epoch_of(parsed.usage.seven_day.expect("weekly window").resets_at),
            1_790_043_558
        );
    }

    #[test]
    fn over_one_hundred_percent_clamps_and_unreadable_percent_fails() {
        let body = br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY",
            "end":"2026-09-22T02:19:18.817992+00:00"},"creditUsagePercent":137}}"#;
        assert_eq!(
            parse_billing(body)
                .unwrap()
                .usage
                .seven_day
                .unwrap()
                .utilization,
            1.0
        );
        for bad in [
            br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY"},"creditUsagePercent":"high"}}"#.as_slice(),
            br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY"},"creditUsagePercent":-1}}"#.as_slice(),
        ] {
            assert!(
                matches!(parse_billing(bad), Err(GrokUsageError::Malformed)),
                "a present-but-unreadable percent must fail the read: {}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn non_billing_bodies_are_malformed_not_empty_state() {
        for body in [
            b"<html>502 Bad Gateway</html>".as_slice(),
            EXPIRED_BODY.as_bytes(), // the live 401 envelope
            b"{}".as_slice(),
            br#"{"config":null}"#.as_slice(),
            br#"{"config":"nope"}"#.as_slice(),
            b"not json".as_slice(),
        ] {
            assert!(
                matches!(parse_billing(body), Err(GrokUsageError::Malformed)),
                "{} must not parse as an empty success",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn errors_are_sanitized_phrases_only() {
        let status = GrokUsageError::Status {
            status: http::StatusCode::UNAUTHORIZED,
        };
        assert_eq!(
            status.sanitized(),
            "upstream returned HTTP 401 Unauthorized"
        );
        assert_eq!(
            GrokUsageError::Malformed.sanitized(),
            "upstream response was not a valid billing document"
        );
        let refused = billing_url("https://user:pw@cli-chat-proxy.grok.com/v1").unwrap_err();
        assert_eq!(
            refused.sanitized(),
            "grok upstream is not a billing endpoint (credentials in the url)",
            "the refusal never echoes the url"
        );
    }
}
