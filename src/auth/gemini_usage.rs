//! Gemini usage source — the Cloud Code model catalog
//! (`POST {base}/v1internal:fetchAvailableModels {"project"}`,
//! docs/gemini/spec.md §R6, trace S6).
//!
//! Live 2026-10-15 (HTTP 200) the document is
//!
//! ```json
//! {"models":{"gemini-3.8-flash-high":{"displayName":…,"maxTokens":1048576,
//!   "maxOutputTokens":65536,
//!   "quotaInfo":{"remainingFraction":1,"resetTime":"2026-10-22T01:50:24Z"},…},
//!   …,
//!   "gemini-4-argon-disabled":{"displayName":"Unavailable","quotaInfo":null,…}},
//!  "defaultAgentModelId":"gemini-3.8-flash-high"}
//! ```
//!
//! Same two rules as [`super::grok_usage`], which this module mirrors:
//!
//! - **Unknown is never zero.** A model without a `quotaInfo` object or
//!   without a parseable `resetTime` yields NO window — not a 0% gauge. The
//!   one exception is upstream's own encoding: Cloud Code speaks proto3 JSON,
//!   which omits zero scalars, so an ABSENT `remainingFraction` next to a
//!   PRESENT reset really is 0 remaining (fully spent) — the same rule
//!   [`super::grok_usage::parse_billing`] applies to `creditUsagePercent`.
//! - **Nothing leaks.** Errors carry a fixed sanitized phrase — never the
//!   response body, the URL, or the bearer token.

use std::time::SystemTime;

use serde_json::{json, Value};

use super::codex_usage::CONTROL_TIMEOUT;
use crate::scheduler::headers::{parse_rfc3339, WindowReading};
use crate::scheduler::usage::{ScopedLimitReading, UsageSnapshot};
use crate::scheduler::window::LimitSeverity;

/// Id suffix of a model the account cannot use (e.g. `gemini-4-argon-disabled`
/// on a Plus account, listed with `quotaInfo: null`).
const DISABLED_SUFFIX: &str = "-disabled";

#[derive(Debug, Clone, thiserror::Error)]
pub enum GeminiUsageError {
    /// Transport failure, reduced to a CLASS (never the source's Display,
    /// which can carry the URL).
    #[error("{0}")]
    Transport(&'static str),
    #[error("upstream returned HTTP {status}")]
    Status { status: http::StatusCode },
    /// Body was not JSON (e.g. an HTML error page) or not a model catalog.
    /// Deliberately does NOT parse into an empty-but-valid state.
    #[error("upstream response was not a valid model quota document")]
    Malformed,
}

impl GeminiUsageError {
    /// Sanitized, operator-facing text. Same as `Display` — spelled out so
    /// call sites can be read as "this is the only thing that reaches a log
    /// or an HTTP body".
    pub fn sanitized(&self) -> String {
        self.to_string()
    }
}

/// One `fetchAvailableModels` observation.
///
/// `usage` carries ONLY what the document proves: the default agent model's
/// quota lands on `seven_day`; every other usable model with a complete quota
/// becomes a scoped reading. `five_hour` is never set (the backend reports no
/// burst window).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GeminiQuota {
    pub usage: UsageSnapshot,
    /// `defaultAgentModelId`, or `None` when the document names none.
    pub default_model: Option<String>,
    /// Every listed model as `(id, remainingFraction, resetTime)`, disabled
    /// ids included (with whatever they carry, usually `(None, None)`), in id
    /// order; an omitted fraction next to a reset reads `Some(0.0)`, and a row
    /// whose fraction is unreadable is left out. Lets a caller look up the
    /// SERVED model's reset instant.
    pub models: Vec<(String, Option<f64>, Option<SystemTime>)>,
}

/// `POST {base}/v1internal:fetchAvailableModels {"project"}` for one gemini
/// account, with the configured Antigravity user agent.
pub async fn fetch_quota(
    client: &reqwest::Client,
    base: &str,
    user_agent: &str,
    access_token: &str,
    project: &str,
) -> Result<GeminiQuota, GeminiUsageError> {
    let base = base.trim().trim_end_matches('/');
    let response = client
        .post(format!("{base}/v1internal:fetchAvailableModels"))
        .bearer_auth(access_token)
        .header(http::header::USER_AGENT, user_agent)
        .header(http::header::ACCEPT, "*/*")
        .json(&json!({ "project": project }))
        .timeout(CONTROL_TIMEOUT)
        .send()
        .await
        .map_err(|e| transport(&e))?;
    let status = response.status();
    if !status.is_success() {
        return Err(GeminiUsageError::Status { status });
    }
    let body = response.bytes().await.map_err(|e| transport(&e))?;
    parse_quota(&body)
}

/// Parse the model catalog.
///
/// STRUCTURE vs CONTENT. The document must carry a `models` OBJECT; anything
/// else (an HTML error page, an error envelope, an unrelated JSON object) is
/// [`GeminiUsageError::Malformed`] rather than an empty success.
///
/// Within that structure a window is derived ONLY from facts present: a
/// model needs a `quotaInfo` OBJECT with a parseable `resetTime`. The
/// default agent model's window is the 7d slot (`utilization = 1 −
/// remainingFraction`); every other non-disabled model with a window becomes
/// a scoped reading labelled by its `displayName`.
///
/// proto3 JSON omits zero scalars, so a `quotaInfo` carrying a reset but NO
/// `remainingFraction` is 0 remaining — utilization 1.0, fully spent (the
/// precedent is [`super::grok_usage::parse_billing`], where an absent
/// `creditUsagePercent` next to a present period is a real 0). A fraction
/// that is present but unreadable (non-numeric, `null`, non-finite or
/// negative) never becomes a fabricated value: that ROW is skipped (logged
/// at debug) and the others still count; only a catalog in which EVERY row
/// is unreadable fails the read as [`GeminiUsageError::Malformed`].
/// `quotaInfo` null/absent, or no parseable reset, stays "no window".
pub fn parse_quota(body: &[u8]) -> Result<GeminiQuota, GeminiUsageError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| GeminiUsageError::Malformed)?;
    let models = value
        .get("models")
        .and_then(Value::as_object)
        .ok_or(GeminiUsageError::Malformed)?;
    let default_model = value
        .get("defaultAgentModelId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut quota = GeminiQuota {
        default_model,
        ..GeminiQuota::default()
    };
    for (id, model) in models {
        let quota_info = model.get("quotaInfo").and_then(Value::as_object);
        let resets_at = quota_info
            .and_then(|info| info.get("resetTime"))
            .and_then(Value::as_str)
            .and_then(parse_rfc3339);
        let remaining = match quota_info.and_then(|info| info.get("remainingFraction")) {
            Some(raw) => match raw.as_f64().filter(|f| f.is_finite() && *f >= 0.0) {
                Some(fraction) => Some(fraction),
                None => {
                    tracing::debug!(model = %id,
                        "gemini quota row skipped: remainingFraction is not a non-negative number");
                    continue;
                }
            },
            // proto3 omitted zero: a reset with no fraction is fully spent.
            None if resets_at.is_some() => Some(0.0),
            None => None,
        };
        quota.models.push((id.clone(), remaining, resets_at));

        let (Some(remaining), Some(resets_at)) = (remaining, resets_at) else {
            continue;
        };
        if id.ends_with(DISABLED_SUFFIX) {
            continue;
        }
        let reading = WindowReading {
            utilization: (1.0 - remaining).clamp(0.0, 1.0),
            resets_at,
        };
        if quota.default_model.as_deref() == Some(id.as_str()) {
            quota.usage.seven_day = Some(reading);
        } else {
            quota.usage.scoped.push(ScopedLimitReading {
                scope_label: display_name(model, id),
                reading,
                severity: LimitSeverity::Normal,
                is_active: false,
            });
        }
    }
    // Every listed row was unreadable: nothing in the document is usable, so
    // it is not a model catalog this parser understands.
    if !models.is_empty() && quota.models.is_empty() {
        return Err(GeminiUsageError::Malformed);
    }
    Ok(quota)
}

/// A model's `displayName`, else its id.
fn display_name(model: &Value, id: &str) -> String {
    model
        .get("displayName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(id)
        .to_string()
}

/// Transport failures reduced to a fixed phrase per class: `reqwest::Error`'s
/// own `Display` can carry the request URL, so it never reaches a caller.
fn transport(err: &reqwest::Error) -> GeminiUsageError {
    GeminiUsageError::Transport(if err.is_timeout() {
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

    /// Shaped after the 2026-10-15 live `fetchAvailableModels` probe (spec
    /// "Model catalog + quota" / "Argon availability" rows): three flash
    /// levels with `remainingFraction:1` resetting 2026-10-22T01:50:24Z, the
    /// disabled argon row with `quotaInfo:null`, default = flash-high. The
    /// flash display names are illustrative (the probe recorded no verbatim
    /// body).
    const LIVE_SHAPED: &str = r#"{"models":{
        "gemini-3.8-flash-high":{"displayName":"Gemini 3.8 Flash (High)","maxTokens":1048576,"maxOutputTokens":65536,
            "quotaInfo":{"remainingFraction":1,"resetTime":"2026-10-22T01:50:24Z"}},
        "gemini-3.8-flash-medium":{"displayName":"Gemini 3.8 Flash (Medium)","maxTokens":1048576,"maxOutputTokens":65536,
            "quotaInfo":{"remainingFraction":1,"resetTime":"2026-10-22T01:50:24Z"}},
        "gemini-3.8-flash-low":{"displayName":"Gemini 3.8 Flash (Low)","maxTokens":1048576,"maxOutputTokens":65536,
            "quotaInfo":{"remainingFraction":1,"resetTime":"2026-10-22T01:50:24Z"}},
        "gemini-4-argon-disabled":{"displayName":"Unavailable","maxTokens":1048576,"maxOutputTokens":65536,
            "quotaInfo":null}},
        "defaultAgentModelId":"gemini-3.8-flash-high"}"#;

    /// `date -u -j -f "%Y-%m-%dT%H:%M:%SZ" "2026-10-22T01:50:24Z" +%s`.
    const RESET_EPOCH: u64 = 1_792_633_824;

    fn epoch_of(at: SystemTime) -> u64 {
        at.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs()
    }

    // ---- G12: quota mapping ----
    #[test]
    fn g12_default_model_maps_to_seven_day_and_others_to_scoped_rows() {
        let quota = parse_quota(LIVE_SHAPED.as_bytes()).unwrap();
        assert_eq!(
            quota.default_model.as_deref(),
            Some("gemini-3.8-flash-high")
        );

        let seven = quota.usage.seven_day.expect("default model window");
        assert_eq!(seven.utilization, 0.0, "remainingFraction 1 → 0% used");
        assert_eq!(epoch_of(seven.resets_at), RESET_EPOCH);
        assert!(quota.usage.five_hour.is_none(), "no burst window upstream");

        let labels: Vec<&str> = quota
            .usage
            .scoped
            .iter()
            .map(|s| s.scope_label.as_str())
            .collect();
        // serde_json's map is key-ordered: flash-low sorts before flash-medium.
        assert_eq!(
            labels,
            ["Gemini 3.8 Flash (Low)", "Gemini 3.8 Flash (Medium)"],
            "the default model is not repeated; the disabled row yields nothing"
        );
        for scoped in &quota.usage.scoped {
            assert_eq!(scoped.reading.utilization, 0.0);
            assert_eq!(epoch_of(scoped.reading.resets_at), RESET_EPOCH);
            assert_eq!(scoped.severity, LimitSeverity::Normal);
            assert!(!scoped.is_active);
        }

        let ids: Vec<&str> = quota.models.iter().map(|(id, _, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "gemini-3.8-flash-high",
                "gemini-3.8-flash-low",
                "gemini-3.8-flash-medium",
                "gemini-4-argon-disabled",
            ],
            "every listed model, disabled included, for served-model lookups"
        );
        let disabled = &quota.models[3];
        assert_eq!((disabled.1, disabled.2), (None, None));
        let high = &quota.models[0];
        assert_eq!(high.1, Some(1.0));
        assert_eq!(high.2.map(epoch_of), Some(RESET_EPOCH));
    }

    #[test]
    fn g12_utilization_is_one_minus_remaining_clamped() {
        let body = br#"{"models":{
            "d":{"quotaInfo":{"remainingFraction":0.25,"resetTime":"2026-10-22T01:50:24Z"}},
            "over":{"displayName":"Over","quotaInfo":{"remainingFraction":1.5,"resetTime":"2026-10-22T01:50:24Z"}},
            "under":{"quotaInfo":{"remainingFraction":0,"resetTime":"2026-10-22T01:50:24Z"}}},
            "defaultAgentModelId":"d"}"#;
        let quota = parse_quota(body).unwrap();
        assert!((quota.usage.seven_day.unwrap().utilization - 0.75).abs() < 1e-9);
        let by_label = |label: &str| {
            quota
                .usage
                .scoped
                .iter()
                .find(|s| s.scope_label == label)
                .map(|s| s.reading.utilization)
        };
        assert_eq!(by_label("Over"), Some(0.0));
        assert_eq!(
            by_label("under"),
            Some(1.0),
            "no displayName → the id labels the row"
        );
    }

    #[test]
    fn g12_a_disabled_model_with_quota_still_yields_no_row() {
        let body = br#"{"models":{"x-disabled":{"displayName":"Unavailable",
            "quotaInfo":{"remainingFraction":0.5,"resetTime":"2026-10-22T01:50:24Z"}}}}"#;
        let quota = parse_quota(body).unwrap();
        assert_eq!(quota.usage, UsageSnapshot::default());
        assert_eq!(quota.models.len(), 1, "still listed");
    }

    // ---- G12b: unknown is never zero ----
    #[test]
    fn g12b_missing_quota_facts_yield_no_window_never_zero() {
        for body in [
            // no quotaInfo at all
            br#"{"models":{"d":{"displayName":"D"}},"defaultAgentModelId":"d"}"#.as_slice(),
            // quotaInfo null
            br#"{"models":{"d":{"quotaInfo":null}},"defaultAgentModelId":"d"}"#.as_slice(),
            // quotaInfo object with neither fact (no reset ⇒ no omitted-zero)
            br#"{"models":{"d":{"quotaInfo":{}}},"defaultAgentModelId":"d"}"#.as_slice(),
            // resetTime missing
            br#"{"models":{"d":{"quotaInfo":{"remainingFraction":0.5}}},"defaultAgentModelId":"d"}"#.as_slice(),
            // resetTime unparseable
            br#"{"models":{"d":{"quotaInfo":{"remainingFraction":0.5,"resetTime":"soon"}}},"defaultAgentModelId":"d"}"#.as_slice(),
            // resetTime unparseable AND fraction omitted: still unknown, not spent
            br#"{"models":{"d":{"quotaInfo":{"resetTime":"soon"}}},"defaultAgentModelId":"d"}"#.as_slice(),
        ] {
            let quota = parse_quota(body).unwrap();
            assert_eq!(
                quota.usage,
                UsageSnapshot::default(),
                "absent, not zero: {}",
                String::from_utf8_lossy(body)
            );
        }

        // No defaultAgentModelId: nothing claims the 7d slot.
        let body = br#"{"models":{"d":{"quotaInfo":{"remainingFraction":0.5,
            "resetTime":"2026-10-22T01:50:24Z"}}}}"#;
        let quota = parse_quota(body).unwrap();
        assert!(quota.usage.seven_day.is_none());
        assert_eq!(quota.default_model, None);
        assert_eq!(quota.usage.scoped.len(), 1);
    }

    #[test]
    fn g12b_omitted_remaining_fraction_is_proto3_zero_fully_spent() {
        // proto3 JSON drops `remainingFraction: 0` — a reset with no fraction
        // is an exhausted model, not an unknown one.
        let body = br#"{"models":{
            "d":{"quotaInfo":{"resetTime":"2026-10-22T01:50:24Z"}},
            "other":{"displayName":"Other","quotaInfo":{"resetTime":"2026-10-22T01:50:24Z"}}},
            "defaultAgentModelId":"d"}"#;
        let quota = parse_quota(body).unwrap();
        let seven = quota.usage.seven_day.expect("omitted zero is a window");
        assert_eq!(seven.utilization, 1.0, "fully spent");
        assert_eq!(epoch_of(seven.resets_at), RESET_EPOCH);
        assert_eq!(quota.usage.scoped.len(), 1);
        assert_eq!(quota.usage.scoped[0].scope_label, "Other");
        assert_eq!(quota.usage.scoped[0].reading.utilization, 1.0);
        let d = quota.models.iter().find(|(id, _, _)| id == "d").unwrap();
        assert_eq!(d.1, Some(0.0), "the served-model lookup sees 0 remaining");
        assert_eq!(d.2.map(epoch_of), Some(RESET_EPOCH));
    }

    #[test]
    fn g12b_unreadable_remaining_fraction_skips_only_that_row() {
        // One unreadable row — non-numeric, null, negative — next to a good
        // one: the good row's window is reported, the bad row yields nothing
        // (not even a served-model entry), never a fabricated value.
        for bad in [r#""high""#, "null", "-0.5"] {
            let body = format!(
                r#"{{"models":{{
                "d":{{"quotaInfo":{{"remainingFraction":0.25,"resetTime":"2026-10-22T01:50:24Z"}}}},
                "x":{{"displayName":"X","quotaInfo":{{"remainingFraction":{bad},"resetTime":"2026-10-22T01:50:24Z"}}}}}},
                "defaultAgentModelId":"d"}}"#
            );
            let quota = parse_quota(body.as_bytes()).unwrap_or_else(|err| panic!("{bad}: {err}"));
            let seven = quota.usage.seven_day.expect("the good row's window");
            assert!((seven.utilization - 0.75).abs() < 1e-9, "{bad}");
            assert_eq!(epoch_of(seven.resets_at), RESET_EPOCH);
            assert!(
                quota.usage.scoped.is_empty(),
                "{bad}: the bad row yields no window"
            );
            let ids: Vec<&str> = quota.models.iter().map(|(id, _, _)| id.as_str()).collect();
            assert_eq!(ids, ["d"], "{bad}: the bad row is skipped");
        }

        // The bad row is the DEFAULT model: the 7d slot stays unknown, the
        // other model's scoped row still lands.
        let body = br#"{"models":{
            "d":{"quotaInfo":{"remainingFraction":"n/a","resetTime":"2026-10-22T01:50:24Z"}},
            "o":{"displayName":"O","quotaInfo":{"remainingFraction":0.5,"resetTime":"2026-10-22T01:50:24Z"}}},
            "defaultAgentModelId":"d"}"#;
        let quota = parse_quota(body).unwrap();
        assert!(quota.usage.seven_day.is_none(), "unknown is never zero");
        assert_eq!(quota.usage.scoped.len(), 1);
        assert_eq!(quota.usage.scoped[0].scope_label, "O");
    }

    #[test]
    fn g12b_a_catalog_whose_every_row_is_unreadable_fails_the_read() {
        for body in [
            br#"{"models":{"d":{"quotaInfo":{"remainingFraction":"high","resetTime":"2026-10-22T01:50:24Z"}}},"defaultAgentModelId":"d"}"#.as_slice(),
            br#"{"models":{"d":{"quotaInfo":{"remainingFraction":null,"resetTime":"2026-10-22T01:50:24Z"}}},"defaultAgentModelId":"d"}"#.as_slice(),
            br#"{"models":{"d":{"quotaInfo":{"remainingFraction":-0.5,"resetTime":"2026-10-22T01:50:24Z"}}},"defaultAgentModelId":"d"}"#.as_slice(),
            br#"{"models":{"d":{"quotaInfo":{"remainingFraction":-1}},
                "x-disabled":{"quotaInfo":{"remainingFraction":"n/a"}}},"defaultAgentModelId":"d"}"#.as_slice(),
        ] {
            assert!(
                matches!(parse_quota(body), Err(GeminiUsageError::Malformed)),
                "no readable row must fail the read: {}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn g12b_non_catalog_bodies_are_malformed_not_empty_state() {
        for body in [
            b"<html><body>502 Bad Gateway</body></html>".as_slice(),
            br#"{"error":{"code":403,"status":"PERMISSION_DENIED"}}"#.as_slice(),
            b"{}".as_slice(),
            br#"{"models":null}"#.as_slice(),
            br#"{"models":[]}"#.as_slice(),
            b"not json".as_slice(),
        ] {
            assert!(
                matches!(parse_quota(body), Err(GeminiUsageError::Malformed)),
                "{} must not parse as an empty success",
                String::from_utf8_lossy(body)
            );
        }
        // An empty catalog is structurally valid: no windows, no fabrication.
        let empty = parse_quota(br#"{"models":{}}"#).unwrap();
        assert_eq!(empty, GeminiQuota::default());
    }

    #[test]
    fn errors_are_sanitized_phrases_only() {
        let status = GeminiUsageError::Status {
            status: http::StatusCode::UNAUTHORIZED,
        };
        assert_eq!(
            status.sanitized(),
            "upstream returned HTTP 401 Unauthorized"
        );
        assert_eq!(
            GeminiUsageError::Malformed.sanitized(),
            "upstream response was not a valid model quota document"
        );
    }

    /// One request as the mock saw it: (path, authorization, user agent, body).
    type SeenRequest = (String, String, String, String);
    type SeenLog = std::sync::Arc<std::sync::Mutex<Vec<SeenRequest>>>;

    /// Local mock (never Google): records the request, answers `status/body`.
    async fn mock_models(status: u16, body: &'static str) -> (std::net::SocketAddr, SeenLog) {
        use std::sync::Arc;
        let seen: SeenLog = Arc::default();
        let seen_in_handler = Arc::clone(&seen);
        let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let seen = Arc::clone(&seen_in_handler);
            async move {
                let (parts, req_body) = req.into_parts();
                let header = |name: http::header::HeaderName| {
                    parts
                        .headers
                        .get(name)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string()
                };
                let auth = header(http::header::AUTHORIZATION);
                let agent = header(http::header::USER_AGENT);
                let path = parts.uri.path().to_string();
                let bytes = axum::body::to_bytes(req_body, usize::MAX)
                    .await
                    .expect("body");
                seen.lock().expect("lock").push((
                    path,
                    auth,
                    agent,
                    String::from_utf8_lossy(&bytes).into_owned(),
                ));
                (
                    http::StatusCode::from_u16(status).expect("status"),
                    [(http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, seen)
    }

    #[tokio::test]
    async fn g12_fetch_quota_posts_the_project_with_bearer_and_user_agent() {
        let (addr, seen) = mock_models(200, LIVE_SHAPED).await;
        let client = reqwest::Client::new();
        let quota = fetch_quota(
            &client,
            &format!("http://{addr}/"),
            "antigravity/hub/2.23.1 darwin/arm64",
            "at-1",
            "proj-1",
        )
        .await
        .expect("quota");
        assert!(quota.usage.seven_day.is_some());
        let seen = seen.lock().expect("lock").clone();
        assert_eq!(seen.len(), 1);
        let (path, auth, agent, body) = &seen[0];
        assert_eq!(path, "/v1internal:fetchAvailableModels");
        assert_eq!(auth, "Bearer at-1");
        assert_eq!(agent, "antigravity/hub/2.23.1 darwin/arm64");
        let body: Value = serde_json::from_str(body).expect("json body");
        assert_eq!(body, json!({"project":"proj-1"}));
    }

    #[tokio::test]
    async fn g12b_fetch_quota_status_and_html_never_leak() {
        let client = reqwest::Client::new();
        let (addr, _seen) = mock_models(401, r#"{"error":{"code":401}}"#).await;
        let err = fetch_quota(&client, &format!("http://{addr}"), "ua", "at-secret", "p")
            .await
            .expect_err("401");
        assert!(matches!(
            err,
            GeminiUsageError::Status { status } if status == http::StatusCode::UNAUTHORIZED
        ));
        assert!(!err.sanitized().contains("at-secret"));
        assert!(!err.sanitized().contains(&addr.to_string()), "no url");

        let (addr, _seen) = mock_models(200, "<html>oops</html>").await;
        let err = fetch_quota(&client, &format!("http://{addr}"), "ua", "at", "p")
            .await
            .expect_err("html");
        assert!(matches!(err, GeminiUsageError::Malformed));

        // Nothing listening: a transport CLASS, never the url.
        let closed = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr")
        };
        let err = fetch_quota(&client, &format!("http://{closed}"), "ua", "at", "p")
            .await
            .expect_err("closed port");
        assert!(matches!(err, GeminiUsageError::Transport(_)), "{err}");
        assert!(!err.sanitized().contains(&closed.to_string()), "no url");
    }
}
