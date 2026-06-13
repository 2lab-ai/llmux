//! Rate-limit header parsing: undocumented unified headers
//! (`anthropic-ratelimit-unified-{5h,7d}-{utilization,reset}`, reset = epoch
//! seconds), documented API-key headers
//! (`anthropic-ratelimit-{requests,tokens}-*`, reset = RFC3339), and codex
//! quota headers (`x-codex-{primary,secondary}-{used-percent,window-minutes,
//! reset-at}`, used-percent = 0–100, reset-at = epoch seconds; live capture
//! 2026-06-12: primary window-minutes 300 = the 5h window, secondary 10080 =
//! the 7d window). Three parsers — the formats differ.
//!
//! Tolerance contract: malformed or missing values are skipped, never an
//! error — headers are an optional evidence source and the 429 path is the
//! always-true fallback.

use std::time::{Duration, SystemTime};

use http::HeaderMap;

/// `anthropic-ratelimit-unified-status` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnifiedStatus {
    Allowed,
    AllowedWarning,
    Rejected,
}

/// One window reading as it appears in headers / the usage endpoint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowReading {
    /// Utilization 0.0..=1.0.
    pub utilization: f64,
    pub resets_at: SystemTime,
}

/// Documented API-key rate-limit headers (token bucket).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StandardRateLimit {
    pub requests_limit: Option<u64>,
    pub requests_remaining: Option<u64>,
    pub requests_reset: Option<SystemTime>,
    pub tokens_limit: Option<u64>,
    pub tokens_remaining: Option<u64>,
    pub tokens_reset: Option<SystemTime>,
}

impl StandardRateLimit {
    /// Derived utilization `1 - remaining/limit` for the requests bucket.
    pub fn requests_utilization(&self) -> Option<f64> {
        derived_utilization(self.requests_limit, self.requests_remaining)
    }

    /// Derived utilization `1 - remaining/limit` for the tokens bucket.
    pub fn tokens_utilization(&self) -> Option<f64> {
        derived_utilization(self.tokens_limit, self.tokens_remaining)
    }

    /// The most-constrained bucket as a window reading, so API-key accounts
    /// get proactive scheduling too. Requires both a derivable utilization
    /// and a reset timestamp for the chosen bucket.
    pub fn as_window_reading(&self) -> Option<WindowReading> {
        let requests =
            self.requests_utilization()
                .zip(self.requests_reset)
                .map(|(utilization, resets_at)| WindowReading {
                    utilization,
                    resets_at,
                });
        let tokens =
            self.tokens_utilization()
                .zip(self.tokens_reset)
                .map(|(utilization, resets_at)| WindowReading {
                    utilization,
                    resets_at,
                });
        match (requests, tokens) {
            (Some(r), Some(t)) => Some(if t.utilization >= r.utilization { t } else { r }),
            (Some(r), None) => Some(r),
            (None, Some(t)) => Some(t),
            (None, None) => None,
        }
    }
}

fn derived_utilization(limit: Option<u64>, remaining: Option<u64>) -> Option<f64> {
    let limit = limit?;
    let remaining = remaining?;
    if limit == 0 {
        return None;
    }
    Some((1.0 - remaining as f64 / limit as f64).clamp(0.0, 1.0))
}

/// Everything quota-relevant extracted from one upstream response.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ParsedRateLimitHeaders {
    pub five_hour: Option<WindowReading>,
    pub seven_day: Option<WindowReading>,
    pub unified_status: Option<UnifiedStatus>,
    pub standard: Option<StandardRateLimit>,
}

impl ParsedRateLimitHeaders {
    /// True when nothing quota-relevant was present (e.g. error responses
    /// from intermediaries) — callers skip recording in that case.
    pub fn is_empty(&self) -> bool {
        self.five_hour.is_none()
            && self.seven_day.is_none()
            && self.unified_status.is_none()
            && self.standard.is_none()
    }
}

/// Parse all known rate-limit headers from an upstream response. Unknown or
/// malformed values are skipped, never an error. Anthropic unified windows
/// and codex `x-codex-*` windows fill the same 5h/7d slots (they never
/// coexist on one response — different upstreams); unified wins if both
/// are somehow present.
pub fn parse(headers: &HeaderMap) -> ParsedRateLimitHeaders {
    let (codex_five, codex_seven) = codex_windows(headers);
    ParsedRateLimitHeaders {
        five_hour: unified_window(headers, "5h").or(codex_five),
        seven_day: unified_window(headers, "7d").or(codex_seven),
        unified_status: header_str(headers, "anthropic-ratelimit-unified-status")
            .and_then(parse_unified_status),
        standard: parse_standard(headers),
    }
}

/// `window-minutes` at or under this maps a codex window into the 5h slot;
/// anything longer (the observed weekly value is 10080) maps into the 7d
/// slot. The observed session value is 300 (= 5h exactly); a day of slack
/// absorbs plan-dependent variations without misfiling the weekly window.
const CODEX_FIVE_HOUR_SLOT_MAX_MINUTES: u64 = 1440;

/// Codex quota windows from `x-codex-{primary,secondary}-*` headers, mapped
/// into the (5h, 7d) scheduler slots by `window-minutes` (live capture:
/// primary 300 → 5h, secondary 10080 → 7d). When `window-minutes` is absent
/// the observed roles are assumed: primary → 5h, secondary → 7d. Exact
/// header names only — `x-codex-bengalfox-*` model-specific variants are
/// deliberately not read.
fn codex_windows(headers: &HeaderMap) -> (Option<WindowReading>, Option<WindowReading>) {
    let mut five_hour = None;
    let mut seven_day = None;
    for (which, five_hour_by_default) in [("primary", true), ("secondary", false)] {
        let Some((reading, window_minutes)) = codex_window(headers, which) else {
            continue;
        };
        let is_five_hour = match window_minutes {
            Some(minutes) => minutes <= CODEX_FIVE_HOUR_SLOT_MAX_MINUTES,
            None => five_hour_by_default,
        };
        let slot = if is_five_hour {
            &mut five_hour
        } else {
            &mut seven_day
        };
        if slot.is_none() {
            *slot = Some(reading);
        }
    }
    (five_hour, seven_day)
}

/// One codex window needs BOTH a parseable `used-percent` and a parseable
/// `reset-at` (same contract as unified windows); `window-minutes` is
/// optional and only steers slot mapping.
fn codex_window(headers: &HeaderMap, which: &str) -> Option<(WindowReading, Option<u64>)> {
    let used_percent = header_f64(headers, &format!("x-codex-{which}-used-percent"))?;
    if !used_percent.is_finite() {
        return None;
    }
    let resets_at =
        header_str(headers, &format!("x-codex-{which}-reset-at")).and_then(parse_epoch_seconds)?;
    let window_minutes = header_u64(headers, &format!("x-codex-{which}-window-minutes"));
    Some((
        WindowReading {
            utilization: (used_percent / 100.0).clamp(0.0, 1.0),
            resets_at,
        },
        window_minutes,
    ))
}

/// One unified window needs BOTH a parseable utilization and a parseable
/// reset; a half-present window is treated as absent.
fn unified_window(headers: &HeaderMap, window: &str) -> Option<WindowReading> {
    let utilization = header_f64(
        headers,
        &format!("anthropic-ratelimit-unified-{window}-utilization"),
    )?;
    if !utilization.is_finite() {
        return None;
    }
    let resets_at = header_str(
        headers,
        &format!("anthropic-ratelimit-unified-{window}-reset"),
    )
    .and_then(parse_epoch_seconds)?;
    Some(WindowReading {
        utilization: utilization.clamp(0.0, 1.0),
        resets_at,
    })
}

fn parse_unified_status(value: &str) -> Option<UnifiedStatus> {
    match value.trim() {
        "allowed" => Some(UnifiedStatus::Allowed),
        "allowed_warning" => Some(UnifiedStatus::AllowedWarning),
        "rejected" => Some(UnifiedStatus::Rejected),
        _ => None,
    }
}

fn parse_standard(headers: &HeaderMap) -> Option<StandardRateLimit> {
    let standard = StandardRateLimit {
        requests_limit: header_u64(headers, "anthropic-ratelimit-requests-limit"),
        requests_remaining: header_u64(headers, "anthropic-ratelimit-requests-remaining"),
        requests_reset: header_str(headers, "anthropic-ratelimit-requests-reset")
            .and_then(parse_rfc3339),
        tokens_limit: header_u64(headers, "anthropic-ratelimit-tokens-limit"),
        tokens_remaining: header_u64(headers, "anthropic-ratelimit-tokens-remaining"),
        tokens_reset: header_str(headers, "anthropic-ratelimit-tokens-reset")
            .and_then(parse_rfc3339),
    };
    let any = standard.requests_limit.is_some()
        || standard.requests_remaining.is_some()
        || standard.requests_reset.is_some()
        || standard.tokens_limit.is_some()
        || standard.tokens_remaining.is_some()
        || standard.tokens_reset.is_some();
    any.then_some(standard)
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok().map(str::trim)
}

fn header_f64(headers: &HeaderMap, name: &str) -> Option<f64> {
    header_str(headers, name)?.parse().ok()
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    header_str(headers, name)?.parse().ok()
}

/// Unified `-reset` values are epoch SECONDS (may be fractional in theory —
/// accept both). Negative / NaN / absurd values are rejected.
pub(crate) fn parse_epoch_seconds(value: &str) -> Option<SystemTime> {
    let secs: f64 = value.parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    let offset = Duration::try_from_secs_f64(secs).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(offset)
}

/// Minimal RFC3339 → `SystemTime` parser (UTC math, `Z` or `±HH:MM` offsets,
/// optional fractional seconds). The standard `-reset` headers are RFC3339;
/// pulling in chrono/time for one format is not worth the tree weight.
/// Returns `None` on anything malformed or pre-epoch.
pub(crate) fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let b = s.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: u64 = s.get(11..13)?.parse().ok()?;
    let minute: u64 = s.get(14..16)?.parse().ok()?;
    let second: u64 = s.get(17..19)?.parse().ok()?;
    if b[4] != b'-'
        || b[7] != b'-'
        || !(b[10] == b'T' || b[10] == b't')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // Leap second: clamp :60 to :59 rather than reject.
    let second = second.min(59);

    // Optional fractional seconds.
    let mut idx = 19;
    let mut nanos: u32 = 0;
    if b.get(idx) == Some(&b'.') {
        idx += 1;
        let frac_start = idx;
        while idx < b.len() && b[idx].is_ascii_digit() {
            idx += 1;
        }
        if idx == frac_start {
            return None;
        }
        let digits = &s[frac_start..idx];
        let kept = digits.get(..digits.len().min(9))?;
        let parsed: u32 = kept.parse().ok()?;
        let scale = 10u32.pow(9 - kept.len() as u32);
        nanos = parsed.checked_mul(scale)?;
    }

    // Offset: Z / z / ±HH:MM.
    let offset_secs: i64 = match b.get(idx) {
        Some(b'Z') | Some(b'z') if idx + 1 == b.len() => 0,
        Some(sign @ (b'+' | b'-')) if idx + 6 == b.len() && b[idx + 3] == b':' => {
            let oh: i64 = s.get(idx + 1..idx + 3)?.parse().ok()?;
            let om: i64 = s.get(idx + 4..idx + 6)?.parse().ok()?;
            if oh > 23 || om > 59 {
                return None;
            }
            let total = oh * 3600 + om * 60;
            if *sign == b'-' {
                -total
            } else {
                total
            }
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    let epoch = days * 86_400 + (hour * 3600 + minute * 60 + second) as i64 - offset_secs;
    if epoch < 0 {
        return None;
    }
    SystemTime::UNIX_EPOCH.checked_add(Duration::new(epoch as u64, nanos))
}

/// Days since 1970-01-01 (proleptic Gregorian) — Howard Hinnant's
/// `days_from_civil` algorithm.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{HeaderName, HeaderValue};

    fn map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                name.parse::<HeaderName>().unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn parses_full_unified_headers() {
        // Real shape per anthropics/claude-code#34074: utilization fraction,
        // reset epoch seconds, status enum.
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-unified-5h-utilization", "0.42"),
            ("anthropic-ratelimit-unified-5h-reset", "1765500000"),
            ("anthropic-ratelimit-unified-7d-utilization", "0.87"),
            ("anthropic-ratelimit-unified-7d-reset", "1765900000"),
            ("anthropic-ratelimit-unified-status", "allowed_warning"),
        ]));
        assert_eq!(
            parsed.five_hour,
            Some(WindowReading {
                utilization: 0.42,
                resets_at: at(1_765_500_000),
            })
        );
        assert_eq!(
            parsed.seven_day,
            Some(WindowReading {
                utilization: 0.87,
                resets_at: at(1_765_900_000),
            })
        );
        assert_eq!(parsed.unified_status, Some(UnifiedStatus::AllowedWarning));
        assert!(parsed.standard.is_none());
        assert!(!parsed.is_empty());
    }

    #[test]
    fn unified_window_requires_both_fields() {
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-unified-5h-utilization", "0.42"),
            // no 5h reset
            ("anthropic-ratelimit-unified-7d-reset", "1765900000"),
            // no 7d utilization
        ]));
        assert!(parsed.five_hour.is_none());
        assert!(parsed.seven_day.is_none());
    }

    #[test]
    fn malformed_values_are_skipped_not_errors() {
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-unified-5h-utilization", "not-a-float"),
            ("anthropic-ratelimit-unified-5h-reset", "1765500000"),
            ("anthropic-ratelimit-unified-7d-utilization", "0.5"),
            ("anthropic-ratelimit-unified-7d-reset", "yesterday"),
            ("anthropic-ratelimit-unified-status", "banana"),
            ("anthropic-ratelimit-requests-limit", "fifty"),
        ]));
        assert!(parsed.is_empty());
    }

    #[test]
    fn utilization_is_clamped_to_unit_range() {
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-unified-5h-utilization", "1.7"),
            ("anthropic-ratelimit-unified-5h-reset", "1765500000"),
            ("anthropic-ratelimit-unified-7d-utilization", "-0.2"),
            ("anthropic-ratelimit-unified-7d-reset", "1765900000"),
        ]));
        assert_eq!(parsed.five_hour.unwrap().utilization, 1.0);
        assert_eq!(parsed.seven_day.unwrap().utilization, 0.0);
    }

    #[test]
    fn negative_or_nan_epoch_rejected() {
        assert!(parse_epoch_seconds("-1").is_none());
        assert!(parse_epoch_seconds("NaN").is_none());
        assert!(parse_epoch_seconds("inf").is_none());
        assert_eq!(parse_epoch_seconds("0"), Some(SystemTime::UNIX_EPOCH));
    }

    #[test]
    fn unified_status_variants() {
        for (raw, expected) in [
            ("allowed", UnifiedStatus::Allowed),
            ("allowed_warning", UnifiedStatus::AllowedWarning),
            ("rejected", UnifiedStatus::Rejected),
        ] {
            let parsed = parse(&map(&[("anthropic-ratelimit-unified-status", raw)]));
            assert_eq!(parsed.unified_status, Some(expected), "{raw}");
        }
    }

    #[test]
    fn parses_standard_api_key_headers() {
        // Real shape per docs.anthropic.com rate-limit docs (RFC3339 reset).
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-requests-limit", "50"),
            ("anthropic-ratelimit-requests-remaining", "49"),
            ("anthropic-ratelimit-requests-reset", "2026-06-12T07:13:19Z"),
            ("anthropic-ratelimit-tokens-limit", "40000"),
            ("anthropic-ratelimit-tokens-remaining", "10000"),
            ("anthropic-ratelimit-tokens-reset", "2026-06-12T07:13:19Z"),
        ]));
        let standard = parsed.standard.unwrap();
        assert_eq!(standard.requests_limit, Some(50));
        assert_eq!(standard.requests_remaining, Some(49));
        assert_eq!(standard.tokens_limit, Some(40_000));
        assert!((standard.requests_utilization().unwrap() - 0.02).abs() < 1e-9);
        assert!((standard.tokens_utilization().unwrap() - 0.75).abs() < 1e-9);
        // Most-constrained bucket (tokens at 75%) drives the window reading.
        let reading = standard.as_window_reading().unwrap();
        assert!((reading.utilization - 0.75).abs() < 1e-9);
        assert_eq!(reading.resets_at, standard.tokens_reset.unwrap());
    }

    #[test]
    fn standard_zero_limit_yields_no_utilization() {
        let standard = StandardRateLimit {
            requests_limit: Some(0),
            requests_remaining: Some(0),
            requests_reset: Some(at(100)),
            tokens_limit: None,
            tokens_remaining: None,
            tokens_reset: None,
        };
        assert!(standard.requests_utilization().is_none());
        assert!(standard.as_window_reading().is_none());
    }

    #[test]
    fn standard_single_bucket_is_enough() {
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-tokens-limit", "1000"),
            ("anthropic-ratelimit-tokens-remaining", "250"),
            ("anthropic-ratelimit-tokens-reset", "2026-06-12T00:00:00Z"),
        ]));
        let reading = parsed.standard.unwrap().as_window_reading().unwrap();
        assert!((reading.utilization - 0.75).abs() < 1e-9);
    }

    // ---- codex x-codex-* quota headers ----

    #[test]
    fn parses_codex_quota_headers_from_live_capture() {
        // Exact values from the 2026-06-12 chatgpt.com smoke capture.
        let parsed = parse(&map(&[
            ("x-codex-primary-used-percent", "0"),
            ("x-codex-secondary-used-percent", "2"),
            ("x-codex-primary-window-minutes", "300"),
            ("x-codex-secondary-window-minutes", "10080"),
            ("x-codex-primary-reset-after-seconds", "275"),
            ("x-codex-secondary-reset-after-seconds", "465379"),
            ("x-codex-primary-reset-at", "1781284314"),
            ("x-codex-secondary-reset-at", "1781749417"),
            ("x-codex-plan-type", "pro"),
            ("x-codex-active-limit", "premium"),
        ]));
        assert_eq!(
            parsed.five_hour,
            Some(WindowReading {
                utilization: 0.0,
                resets_at: at(1_781_284_314),
            }),
            "primary (300 min) is the 5h window"
        );
        assert_eq!(
            parsed.seven_day,
            Some(WindowReading {
                utilization: 0.02,
                resets_at: at(1_781_749_417),
            }),
            "secondary (10080 min) is the 7d window"
        );
        assert!(parsed.unified_status.is_none());
        assert!(parsed.standard.is_none());
        assert!(!parsed.is_empty());
    }

    #[test]
    fn codex_slot_mapping_follows_window_minutes_not_position() {
        // Roles swapped: primary carries the weekly window. Mapping must
        // follow window-minutes, not the primary/secondary label.
        let parsed = parse(&map(&[
            ("x-codex-primary-used-percent", "40"),
            ("x-codex-primary-window-minutes", "10080"),
            ("x-codex-primary-reset-at", "1781749417"),
            ("x-codex-secondary-used-percent", "10"),
            ("x-codex-secondary-window-minutes", "300"),
            ("x-codex-secondary-reset-at", "1781284314"),
        ]));
        assert_eq!(parsed.five_hour.unwrap().utilization, 0.10);
        assert_eq!(parsed.five_hour.unwrap().resets_at, at(1_781_284_314));
        assert_eq!(parsed.seven_day.unwrap().utilization, 0.40);
        assert_eq!(parsed.seven_day.unwrap().resets_at, at(1_781_749_417));
    }

    #[test]
    fn codex_window_minutes_absent_falls_back_to_observed_roles() {
        let parsed = parse(&map(&[
            ("x-codex-primary-used-percent", "37"),
            ("x-codex-primary-reset-at", "1781284314"),
            ("x-codex-secondary-used-percent", "2"),
            ("x-codex-secondary-reset-at", "1781749417"),
        ]));
        assert_eq!(parsed.five_hour.unwrap().utilization, 0.37);
        assert_eq!(parsed.seven_day.unwrap().utilization, 0.02);
    }

    #[test]
    fn codex_window_requires_both_used_percent_and_reset_at() {
        // Missing reset-at → window absent.
        let parsed = parse(&map(&[("x-codex-primary-used-percent", "12")]));
        assert!(parsed.is_empty());
        // Garbage used-percent → window absent (reset alone is not enough).
        let parsed = parse(&map(&[
            ("x-codex-primary-used-percent", "lots"),
            ("x-codex-primary-reset-at", "1781284314"),
        ]));
        assert!(parsed.is_empty());
        // Garbage reset-at → window absent.
        let parsed = parse(&map(&[
            ("x-codex-primary-used-percent", "12"),
            ("x-codex-primary-reset-at", "soon"),
        ]));
        assert!(parsed.is_empty());
    }

    #[test]
    fn codex_used_percent_is_clamped_to_unit_range() {
        let parsed = parse(&map(&[
            ("x-codex-primary-used-percent", "250"),
            ("x-codex-primary-reset-at", "1781284314"),
            ("x-codex-secondary-used-percent", "-5"),
            ("x-codex-secondary-reset-at", "1781749417"),
        ]));
        assert_eq!(parsed.five_hour.unwrap().utilization, 1.0);
        assert_eq!(parsed.seven_day.unwrap().utilization, 0.0);
    }

    #[test]
    fn codex_bengalfox_model_variants_are_ignored() {
        // The capture also carries x-codex-bengalfox-* (per-model limits);
        // only the exact primary/secondary names may be read.
        let parsed = parse(&map(&[
            ("x-codex-bengalfox-primary-used-percent", "90"),
            ("x-codex-bengalfox-primary-reset-at", "1781302039"),
            ("x-codex-bengalfox-secondary-used-percent", "90"),
            ("x-codex-bengalfox-secondary-reset-at", "1781888839"),
        ]));
        assert!(parsed.is_empty());
    }

    #[test]
    fn unified_windows_win_over_codex_windows() {
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-unified-5h-utilization", "0.42"),
            ("anthropic-ratelimit-unified-5h-reset", "1765500000"),
            ("x-codex-primary-used-percent", "90"),
            ("x-codex-primary-reset-at", "1781284314"),
        ]));
        assert_eq!(parsed.five_hour.unwrap().utilization, 0.42);
    }

    #[test]
    fn empty_header_map_is_empty() {
        assert!(parse(&HeaderMap::new()).is_empty());
    }

    #[test]
    fn rfc3339_zulu() {
        // 2026-06-12T00:00:00Z == 1781222400 (independently computed).
        assert_eq!(
            parse_rfc3339("2026-06-12T00:00:00Z"),
            Some(at(1_781_222_400))
        );
    }

    #[test]
    fn rfc3339_epoch_origin() {
        assert_eq!(
            parse_rfc3339("1970-01-01T00:00:00Z"),
            Some(SystemTime::UNIX_EPOCH)
        );
    }

    #[test]
    fn rfc3339_fractional_seconds() {
        assert_eq!(
            parse_rfc3339("1970-01-01T00:00:01.5Z"),
            Some(SystemTime::UNIX_EPOCH + Duration::new(1, 500_000_000))
        );
        assert_eq!(
            parse_rfc3339("1970-01-01T00:00:00.000000001Z"),
            Some(SystemTime::UNIX_EPOCH + Duration::new(0, 1))
        );
    }

    #[test]
    fn rfc3339_positive_offset() {
        // 09:00 KST == 00:00 UTC.
        assert_eq!(
            parse_rfc3339("2026-06-12T09:00:00+09:00"),
            Some(at(1_781_222_400))
        );
    }

    #[test]
    fn rfc3339_negative_offset() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00-01:30"), Some(at(5_400)));
    }

    #[test]
    fn rfc3339_garbage_rejected() {
        for bad in [
            "",
            "tomorrow",
            "2026-13-01T00:00:00Z",
            "2026-06-12T25:00:00Z",
            "2026-06-12T00:00:00",      // no offset
            "2026-06-12T00:00:00+9:00", // malformed offset
            "1969-12-31T23:59:59Z",     // pre-epoch
            "2026-06-12T00:00:00.Z",    // empty fraction
        ] {
            assert!(parse_rfc3339(bad).is_none(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn rfc3339_leap_second_clamped() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:60Z"), Some(at(59)));
    }
}
