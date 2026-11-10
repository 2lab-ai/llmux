//! Rate-limit header parsing: undocumented unified headers
//! (`anthropic-ratelimit-unified-{7d,7d_oi}-{utilization,reset}`, reset =
//! epoch seconds) and codex quota headers (`x-codex-{primary,secondary}-
//! {used-percent,window-minutes,reset-at}`, used-percent = 0–100, reset-at =
//! epoch seconds). Two parsers — the formats differ.
//!
//! Only WEEKLY windows are read. No provider reports a 5-hour window any more
//! (owner, 2026-11-04 and 2026-11-10), so llmux keeps no 5h slot: a stray
//! `unified-5h-*` header, a short codex window (`window-minutes` ≤ a day) and
//! the API-key/grok token-bucket headers (`anthropic-ratelimit-{requests,
//! tokens}-*`, `x-ratelimit-*`, which only ever fed that slot) are ignored.
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

/// Everything quota-relevant extracted from one upstream response.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ParsedRateLimitHeaders {
    pub seven_day: Option<WindowReading>,
    /// The model-scoped weekly bucket from the
    /// `anthropic-ratelimit-unified-7d_oi-*` headers (issue #123; live
    /// capture 2026-07-16: `7d_oi-utilization: 0.0`, `7d_oi-reset` epoch
    /// seconds — the bucket the usage poll reports as the "Fable" weekly
    /// row). Response headers keep the scoped gauge live between polls, so
    /// a COLD gauge heals on the first request through the account.
    pub fable_weekly: Option<WindowReading>,
    pub unified_status: Option<UnifiedStatus>,
}

impl ParsedRateLimitHeaders {
    /// True when nothing quota-relevant was present (e.g. error responses
    /// from intermediaries) — callers skip recording in that case.
    pub fn is_empty(&self) -> bool {
        self.seven_day.is_none() && self.fable_weekly.is_none() && self.unified_status.is_none()
    }
}

/// Parse all known rate-limit headers from an upstream response. Unknown or
/// malformed values are skipped, never an error. The Anthropic unified weekly
/// window and the codex weekly window fill the same 7d slot (they never
/// coexist on one response — different upstreams); unified wins if both are
/// somehow present.
pub fn parse(headers: &HeaderMap) -> ParsedRateLimitHeaders {
    ParsedRateLimitHeaders {
        seven_day: unified_window(headers, "7d").or_else(|| codex_weekly(headers)),
        fable_weekly: unified_window(headers, "7d_oi"),
        unified_status: header_str(headers, "anthropic-ratelimit-unified-status")
            .and_then(parse_unified_status),
    }
}

/// A codex window whose `window-minutes` is at or under this is a short
/// (session) window and is ignored; anything longer (the observed weekly
/// value is 10080) is the weekly window.
const CODEX_SHORT_WINDOW_MAX_MINUTES: u64 = 1440;

/// The codex weekly window from `x-codex-{primary,secondary}-*` headers,
/// chosen by `window-minutes` (the 2026-09-11 capture has the weekly window
/// in `primary`, so position says nothing). When no window states its
/// minutes, the observed weekly role is assumed: `secondary` (the 2026-06-12
/// shape) else a LONE `primary` (the 2026-09-11 shape) — lone meaning no
/// secondary header at all: a secondary that is there but unreadable leaves
/// the slot empty rather than handing it to the session window. Exact header
/// names only — `x-codex-bengalfox-*` model-specific variants are deliberately
/// not read.
fn codex_weekly(headers: &HeaderMap) -> Option<WindowReading> {
    let windows = [
        codex_window(headers, "primary"),
        codex_window(headers, "secondary"),
    ];
    let by_minutes = windows.iter().flatten().find_map(|(reading, minutes)| {
        minutes
            .filter(|m| *m > CODEX_SHORT_WINDOW_MAX_MINUTES)
            .map(|_| *reading)
    });
    let unlabeled = |w: &Option<(WindowReading, Option<u64>)>| {
        w.filter(|(_, minutes)| minutes.is_none())
            .map(|(reading, _)| reading)
    };
    let secondary_absent = ["used-percent", "reset-at", "window-minutes"]
        .iter()
        .all(|field| !headers.contains_key(format!("x-codex-secondary-{field}").as_str()));
    by_minutes
        .or_else(|| unlabeled(&windows[1]))
        .or_else(|| unlabeled(&windows[0]).filter(|_| secondary_absent))
}

/// One codex window needs BOTH a parseable `used-percent` and a parseable
/// `reset-at` (same contract as unified windows); `window-minutes` is
/// optional and only steers which window is the weekly one.
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
/// optional fractional seconds). The usage endpoints' `resets_at` strings are RFC3339;
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
        // reset epoch seconds, status enum. A stray 5h pair is ignored.
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-unified-5h-utilization", "0.42"),
            ("anthropic-ratelimit-unified-5h-reset", "1765500000"),
            ("anthropic-ratelimit-unified-7d-utilization", "0.87"),
            ("anthropic-ratelimit-unified-7d-reset", "1765900000"),
            ("anthropic-ratelimit-unified-status", "allowed_warning"),
        ]));
        assert_eq!(
            parsed.seven_day,
            Some(WindowReading {
                utilization: 0.87,
                resets_at: at(1_765_900_000),
            })
        );
        assert_eq!(parsed.unified_status, Some(UnifiedStatus::AllowedWarning));
        assert!(!parsed.is_empty());
    }

    #[test]
    fn five_hour_headers_alone_are_empty() {
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-unified-5h-utilization", "1.0"),
            ("anthropic-ratelimit-unified-5h-reset", "1765500000"),
        ]));
        assert!(parsed.is_empty(), "{parsed:?}");
    }

    #[test]
    fn unified_window_requires_both_fields() {
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-unified-7d-reset", "1765900000"),
            // no 7d utilization
        ]));
        assert!(parsed.seven_day.is_none());
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-unified-7d-utilization", "0.5"),
            // no 7d reset
        ]));
        assert!(parsed.seven_day.is_none());
    }

    /// Issue #123 (live capture 2026-07-16): the `7d_oi` scoped bucket rides
    /// the same unified header shape — parse it into `fable_weekly` with the
    /// same both-fields-required contract.
    #[test]
    fn unified_7d_oi_window_parses_into_fable_weekly() {
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-unified-7d_oi-utilization", "0.0"),
            ("anthropic-ratelimit-unified-7d_oi-reset", "1784325600"),
        ]));
        let fable = parsed.fable_weekly.expect("7d_oi parsed");
        assert_eq!(fable.utilization, 0.0);
        assert_eq!(
            fable.resets_at,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_784_325_600)
        );
        assert!(
            !parsed.is_empty(),
            "a lone 7d_oi reading counts as quota data"
        );
        // Both-fields contract holds for the scoped bucket too.
        let partial = parse(&map(&[(
            "anthropic-ratelimit-unified-7d_oi-utilization",
            "0.5",
        )]));
        assert!(partial.fable_weekly.is_none());
    }

    #[test]
    fn malformed_values_are_skipped_not_errors() {
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-unified-7d-utilization", "not-a-float"),
            ("anthropic-ratelimit-unified-7d-reset", "1765900000"),
            ("anthropic-ratelimit-unified-7d_oi-utilization", "0.5"),
            ("anthropic-ratelimit-unified-7d_oi-reset", "yesterday"),
            ("anthropic-ratelimit-unified-status", "banana"),
        ]));
        assert!(parsed.is_empty());
    }

    #[test]
    fn utilization_is_clamped_to_unit_range() {
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-unified-7d-utilization", "1.7"),
            ("anthropic-ratelimit-unified-7d-reset", "1765900000"),
            ("anthropic-ratelimit-unified-7d_oi-utilization", "-0.2"),
            ("anthropic-ratelimit-unified-7d_oi-reset", "1765900000"),
        ]));
        assert_eq!(parsed.seven_day.unwrap().utilization, 1.0);
        assert_eq!(parsed.fable_weekly.unwrap().utilization, 0.0);
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
    fn token_bucket_headers_are_not_read() {
        // The API-key and grok token buckets only ever fed the retired 5h
        // slot: a response carrying only them records nothing.
        let parsed = parse(&map(&[
            ("anthropic-ratelimit-requests-limit", "50"),
            ("anthropic-ratelimit-requests-remaining", "0"),
            ("anthropic-ratelimit-requests-reset", "2026-06-12T07:13:19Z"),
            ("anthropic-ratelimit-tokens-limit", "40000"),
            ("anthropic-ratelimit-tokens-remaining", "0"),
            ("anthropic-ratelimit-tokens-reset", "2026-06-12T07:13:19Z"),
            ("x-ratelimit-limit-tokens", "15000000"),
            ("x-ratelimit-remaining-tokens", "0"),
            ("x-ratelimit-limit-requests", "900"),
            ("x-ratelimit-remaining-requests", "0"),
        ]));
        assert!(parsed.is_empty(), "{parsed:?}");
    }

    // ---- codex x-codex-* quota headers ----

    #[test]
    fn parses_codex_quota_headers_from_live_capture() {
        // Exact values from the 2026-06-12 chatgpt.com smoke capture. The
        // 300-minute primary window is a short window and is ignored.
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
            parsed.seven_day,
            Some(WindowReading {
                utilization: 0.02,
                resets_at: at(1_781_749_417),
            }),
            "secondary (10080 min) is the 7d window"
        );
        assert!(parsed.unified_status.is_none());
        assert!(!parsed.is_empty());
    }

    #[test]
    fn codex_weekly_follows_window_minutes_not_position() {
        // The 2026-09-11 shape: primary carries the weekly window.
        let parsed = parse(&map(&[
            ("x-codex-primary-used-percent", "40"),
            ("x-codex-primary-window-minutes", "10080"),
            ("x-codex-primary-reset-at", "1781749417"),
            ("x-codex-secondary-used-percent", "10"),
            ("x-codex-secondary-window-minutes", "300"),
            ("x-codex-secondary-reset-at", "1781284314"),
        ]));
        assert_eq!(parsed.seven_day.unwrap().utilization, 0.40);
        assert_eq!(parsed.seven_day.unwrap().resets_at, at(1_781_749_417));
    }

    #[test]
    fn codex_short_window_alone_is_empty() {
        let parsed = parse(&map(&[
            ("x-codex-primary-used-percent", "100"),
            ("x-codex-primary-window-minutes", "300"),
            ("x-codex-primary-reset-at", "1781284314"),
        ]));
        assert!(parsed.is_empty(), "{parsed:?}");
    }

    #[test]
    fn codex_window_minutes_absent_falls_back_to_observed_roles() {
        // Both present, no minutes: secondary is the weekly role.
        let parsed = parse(&map(&[
            ("x-codex-primary-used-percent", "37"),
            ("x-codex-primary-reset-at", "1781284314"),
            ("x-codex-secondary-used-percent", "2"),
            ("x-codex-secondary-reset-at", "1781749417"),
        ]));
        assert_eq!(parsed.seven_day.unwrap().utilization, 0.02);
        // A lone primary with no minutes is the 2026-09-11 weekly shape.
        let parsed = parse(&map(&[
            ("x-codex-primary-used-percent", "43"),
            ("x-codex-primary-reset-at", "1789442251"),
        ]));
        assert_eq!(parsed.seven_day.unwrap().utilization, 0.43);
    }

    #[test]
    fn codex_lone_primary_fallback_needs_the_secondary_absent_not_broken() {
        // review-of-legends P2 (PR #273): a secondary that is there but unreadable (weekly by its
        // minutes, reset missing) must not hand the weekly slot to an unlabeled primary — that is
        // the session window, and a 100% session reading would mark the account weekly-exhausted.
        let parsed = parse(&map(&[
            ("x-codex-primary-used-percent", "100"),
            ("x-codex-primary-reset-at", "1781284314"),
            ("x-codex-secondary-used-percent", "2"),
            ("x-codex-secondary-window-minutes", "10080"),
        ]));
        assert!(parsed.is_empty(), "{parsed:?}");
        // Same with a garbage secondary reset.
        let parsed = parse(&map(&[
            ("x-codex-primary-used-percent", "100"),
            ("x-codex-primary-reset-at", "1781284314"),
            ("x-codex-secondary-used-percent", "2"),
            ("x-codex-secondary-reset-at", "soon"),
        ]));
        assert!(parsed.is_empty(), "{parsed:?}");
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
            ("x-codex-secondary-used-percent", "250"),
            ("x-codex-secondary-reset-at", "1781749417"),
        ]));
        assert_eq!(parsed.seven_day.unwrap().utilization, 1.0);
        let parsed = parse(&map(&[
            ("x-codex-secondary-used-percent", "-5"),
            ("x-codex-secondary-reset-at", "1781749417"),
        ]));
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
            ("anthropic-ratelimit-unified-7d-utilization", "0.42"),
            ("anthropic-ratelimit-unified-7d-reset", "1765900000"),
            ("x-codex-secondary-used-percent", "90"),
            ("x-codex-secondary-reset-at", "1781284314"),
        ]));
        assert_eq!(parsed.seven_day.unwrap().utilization, 0.42);
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
