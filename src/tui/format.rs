//! Pure formatting helpers for the dashboard: gauge color thresholds,
//! reset-countdown strings, bar rendering, count humanization. No IO, no
//! clock reads — everything takes explicit inputs so it unit-tests cleanly.

use std::time::{Duration, SystemTime};

/// Utilization at/above which a gauge turns yellow.
pub(crate) const GAUGE_YELLOW_AT: f64 = 0.70;
/// Utilization at/above which a gauge turns red.
pub(crate) const GAUGE_RED_AT: f64 = 0.90;

/// Color band for a quota gauge (FR6: green <70%, yellow 70–90%, red ≥90%).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GaugeLevel {
    Green,
    Yellow,
    Red,
}

/// Map utilization (0.0..) to its color band.
pub(crate) fn gauge_level(utilization: f64) -> GaugeLevel {
    if utilization >= GAUGE_RED_AT {
        GaugeLevel::Red
    } else if utilization >= GAUGE_YELLOW_AT {
        GaugeLevel::Yellow
    } else {
        GaugeLevel::Green
    }
}

/// Compact countdown, largest two units: "2d 4h", "2h 13m", "13m 05s", "42s".
pub(crate) fn countdown(remaining: Duration) -> String {
    let total = remaining.as_secs();
    let (days, hours, mins, secs) = (
        total / 86_400,
        (total % 86_400) / 3_600,
        (total % 3_600) / 60,
        total % 60,
    );
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins:02}m")
    } else if mins > 0 {
        format!("{mins}m {secs:02}s")
    } else {
        format!("{secs}s")
    }
}

/// Countdown from `now` to `until`; `None` when `until` is absent or past.
pub(crate) fn countdown_until(until: Option<SystemTime>, now: SystemTime) -> Option<String> {
    let remaining = until?.duration_since(now).ok()?;
    if remaining.is_zero() {
        return None;
    }
    Some(countdown(remaining))
}

/// Fixed-width utilization bar, e.g. `▰▰▰▱▱▱▱▱` (utilization clamped 0..=1).
pub(crate) fn gauge_bar(utilization: f64, width: usize) -> String {
    let clamped = utilization.clamp(0.0, 1.0);
    // Round so 0 < utilization is visible only from ~half a cell up; full
    // width strictly at 100%.
    let filled = ((clamped * width as f64).round() as usize).min(width);
    let mut bar = String::with_capacity(width * 3);
    for _ in 0..filled {
        bar.push('▰');
    }
    for _ in filled..width {
        bar.push('▱');
    }
    bar
}

/// Whole-number percentage label: "42%".
pub(crate) fn percent(utilization: f64) -> String {
    format!("{:.0}%", utilization.clamp(0.0, 1.0) * 100.0)
}

/// Humanized count for the totals column: "999", "1.2k", "3.4M".
pub(crate) fn human_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Single largest-unit age for "refreshed N ago" markers: "42s", "3m",
/// "6h", "2d". Coarser than `select::compact_duration` on purpose — an age
/// marker needs magnitude, not precision.
pub(crate) fn age_unit(age: Duration) -> String {
    let total = age.as_secs();
    if total >= 86_400 {
        format!("{}d", total / 86_400)
    } else if total >= 3_600 {
        format!("{}h", total / 3_600)
    } else if total >= 60 {
        format!("{}m", total / 60)
    } else {
        format!("{total}s")
    }
}

/// "↻3m" refreshed-ago marker for the token column / status line; `None`
/// when the token was never refreshed (`last_refresh_ms` absent). A
/// last-refresh timestamp in the future (clock skew) clamps to "↻0s".
pub(crate) fn refreshed_marker(last_refresh_ms: Option<u64>, now: SystemTime) -> Option<String> {
    let at = SystemTime::UNIX_EPOCH + Duration::from_millis(last_refresh_ms?);
    let ago = now.duration_since(at).unwrap_or_default();
    Some(format!("\u{21bb}{}", age_unit(ago)))
}

/// Wall-clock HH:MM:SS in UTC for activity-log timestamps. UTC (not local)
/// because std has no timezone database and pulling chrono in for a log
/// prefix isn't worth the dependency.
pub(crate) fn clock_hms_utc(at: SystemTime) -> String {
    let secs = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let day = secs % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        day / 3_600,
        (day % 3_600) / 60,
        day % 60
    )
}

/// Elapsed seconds with one decimal, for in-flight entries: "3.2s".
pub(crate) fn elapsed_secs(elapsed: Duration) -> String {
    format!("{:.1}s", elapsed.as_secs_f64())
}

/// Local UTC offset in seconds at `at`, via `localtime_r` (no timezone
/// database in std; libc has one). Non-unix targets degrade to UTC.
#[cfg(unix)]
pub(crate) fn local_offset_secs(at: SystemTime) -> i64 {
    let secs = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let time: libc::time_t = match libc::time_t::try_from(secs) {
        Ok(time) => time,
        Err(_) => return 0,
    };
    // SAFETY: localtime_r is the thread-safe variant; `tm` is a plain C
    // struct fully written by the call when it returns non-null.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::localtime_r(&time, &mut tm) };
    if result.is_null() {
        0
    } else {
        i64::from(tm.tm_gmtoff as i32)
    }
}

#[cfg(not(unix))]
pub(crate) fn local_offset_secs(_at: SystemTime) -> i64 {
    0
}

/// Wall-clock "HH:MM" at a fixed UTC offset — pure, so the absolute-time
/// labels are unit-testable with explicit offsets.
pub(crate) fn clock_hm(at: SystemTime, offset_secs: i64) -> String {
    let epoch = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let local = (epoch as i64).saturating_add(offset_secs);
    let day = local.rem_euclid(86_400);
    format!("{:02}:{:02}", day / 3_600, (day % 3_600) / 60)
}

/// Absolute label for a reset timestamp: "14:30" when it lands within 24h
/// of `now`, "06-15 09:00" (local month-day) when it is further out —
/// a bare clock time would be ambiguous across days.
pub(crate) fn absolute_label(at: SystemTime, now: SystemTime, offset_secs: i64) -> String {
    let within_day = at
        .duration_since(now)
        .map(|d| d < Duration::from_secs(86_400))
        .unwrap_or(true);
    if within_day {
        return clock_hm(at, offset_secs);
    }
    let epoch = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let local = (epoch as i64).saturating_add(offset_secs);
    let (_, month, day) = civil_from_days(local.div_euclid(86_400));
    format!("{:02}-{:02} {}", month, day, clock_hm(at, offset_secs))
}

/// Days-since-epoch → (year, month, day), Howard Hinnant's civil algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- gauge color thresholds (FR6) ----

    #[test]
    fn gauge_green_below_70() {
        assert_eq!(gauge_level(0.0), GaugeLevel::Green);
        assert_eq!(gauge_level(0.42), GaugeLevel::Green);
        assert_eq!(gauge_level(0.699), GaugeLevel::Green);
    }

    #[test]
    fn gauge_yellow_from_70_to_below_90() {
        assert_eq!(gauge_level(0.70), GaugeLevel::Yellow, "70% is yellow");
        assert_eq!(gauge_level(0.85), GaugeLevel::Yellow);
        assert_eq!(gauge_level(0.899), GaugeLevel::Yellow);
    }

    #[test]
    fn gauge_red_at_90_and_above() {
        assert_eq!(gauge_level(0.90), GaugeLevel::Red, "90% is red");
        assert_eq!(gauge_level(1.0), GaugeLevel::Red);
        assert_eq!(
            gauge_level(1.5),
            GaugeLevel::Red,
            "over-reported util stays red"
        );
    }

    // ---- countdown formatting (m/h/d) ----

    #[test]
    fn countdown_seconds_only() {
        assert_eq!(countdown(Duration::from_secs(0)), "0s");
        assert_eq!(countdown(Duration::from_secs(42)), "42s");
        assert_eq!(countdown(Duration::from_secs(59)), "59s");
    }

    #[test]
    fn countdown_minutes_and_seconds() {
        assert_eq!(countdown(Duration::from_secs(60)), "1m 00s");
        assert_eq!(countdown(Duration::from_secs(5 * 60 + 3)), "5m 03s");
        assert_eq!(countdown(Duration::from_secs(59 * 60 + 59)), "59m 59s");
    }

    #[test]
    fn countdown_hours_and_minutes() {
        assert_eq!(countdown(Duration::from_secs(3_600)), "1h 00m");
        // The spec's own example: "2h 13m".
        assert_eq!(
            countdown(Duration::from_secs(2 * 3_600 + 13 * 60)),
            "2h 13m"
        );
        assert_eq!(
            countdown(Duration::from_secs(23 * 3_600 + 59 * 60)),
            "23h 59m"
        );
    }

    #[test]
    fn countdown_days_and_hours() {
        assert_eq!(countdown(Duration::from_secs(86_400)), "1d 0h");
        assert_eq!(
            countdown(Duration::from_secs(2 * 86_400 + 4 * 3_600)),
            "2d 4h"
        );
    }

    #[test]
    fn countdown_until_none_when_past_or_absent() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert_eq!(countdown_until(None, now), None);
        let past = SystemTime::UNIX_EPOCH + Duration::from_secs(999);
        assert_eq!(countdown_until(Some(past), now), None);
        assert_eq!(countdown_until(Some(now), now), None, "exactly now is over");
        let future = now + Duration::from_secs(120);
        assert_eq!(countdown_until(Some(future), now), Some("2m 00s".into()));
    }

    // ---- bar / labels ----

    #[test]
    fn gauge_bar_fills_proportionally_and_clamps() {
        assert_eq!(gauge_bar(0.0, 4), "▱▱▱▱");
        assert_eq!(gauge_bar(0.5, 4), "▰▰▱▱");
        assert_eq!(gauge_bar(1.0, 4), "▰▰▰▰");
        assert_eq!(gauge_bar(7.0, 4), "▰▰▰▰", "clamped above 1.0");
        assert_eq!(gauge_bar(-1.0, 4), "▱▱▱▱", "clamped below 0.0");
        assert_eq!(gauge_bar(0.5, 0), "", "zero width degrades to empty");
    }

    #[test]
    fn percent_rounds_and_clamps() {
        assert_eq!(percent(0.42), "42%");
        assert_eq!(percent(0.999), "100%");
        assert_eq!(percent(2.0), "100%");
    }

    #[test]
    fn human_count_bands() {
        assert_eq!(human_count(0), "0");
        assert_eq!(human_count(999), "999");
        assert_eq!(human_count(1_200), "1.2k");
        assert_eq!(human_count(999_949), "999.9k");
        assert_eq!(human_count(3_400_000), "3.4M");
    }

    // ---- refreshed-ago marker (token column / status line) ----

    #[test]
    fn age_unit_is_largest_single_unit() {
        assert_eq!(age_unit(Duration::from_secs(0)), "0s");
        assert_eq!(age_unit(Duration::from_secs(42)), "42s");
        assert_eq!(age_unit(Duration::from_secs(3 * 60 + 12)), "3m");
        assert_eq!(age_unit(Duration::from_secs(6 * 3_600 + 52 * 60)), "6h");
        assert_eq!(age_unit(Duration::from_secs(2 * 86_400 + 3_600)), "2d");
    }

    #[test]
    fn refreshed_marker_renders_age_or_none() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert_eq!(refreshed_marker(None, now), None, "never refreshed");
        let three_min_ago = 1_000_000_000 - 3 * 60 * 1000;
        assert_eq!(
            refreshed_marker(Some(three_min_ago), now),
            Some("\u{21bb}3m".into())
        );
        // Future timestamp (clock skew) clamps to zero instead of panicking.
        assert_eq!(
            refreshed_marker(Some(1_000_000_000 + 5_000), now),
            Some("\u{21bb}0s".into())
        );
    }

    #[test]
    fn clock_is_utc_hms() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(3_661);
        assert_eq!(clock_hms_utc(at), "01:01:01");
        let midnight = SystemTime::UNIX_EPOCH + Duration::from_secs(2 * 86_400);
        assert_eq!(clock_hms_utc(midnight), "00:00:00");
    }

    #[test]
    fn elapsed_one_decimal() {
        assert_eq!(elapsed_secs(Duration::from_millis(3_240)), "3.2s");
    }

    // ---- absolute local time ----

    #[test]
    fn clock_hm_applies_offset() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(12 * 3_600 + 30 * 60); // 12:30 UTC
        assert_eq!(clock_hm(at, 0), "12:30");
        assert_eq!(clock_hm(at, 9 * 3_600), "21:30", "UTC+9");
        assert_eq!(clock_hm(at, -5 * 3_600), "07:30", "UTC-5");
        // Offset wrapping across midnight.
        let late = SystemTime::UNIX_EPOCH + Duration::from_secs(23 * 3_600);
        assert_eq!(clock_hm(late, 2 * 3_600), "01:00");
    }

    #[test]
    fn absolute_label_within_a_day_is_clock_only() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let soon = now + Duration::from_secs(3_720);
        assert_eq!(absolute_label(soon, now, 0), clock_hm(soon, 0));
        // Past timestamps degrade to clock form too (no negative dates).
        assert_eq!(absolute_label(now, now, 0), clock_hm(now, 0));
    }

    #[test]
    fn absolute_label_beyond_a_day_includes_the_date() {
        // 2026-06-13 00:00:00 UTC = 1781308800.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_781_308_800);
        let in_two_days = now + Duration::from_secs(2 * 86_400 + 9 * 3_600);
        assert_eq!(absolute_label(in_two_days, now, 0), "06-15 09:00");
        // UTC+9 pushes it past the next local midnight.
        let in_30h = now + Duration::from_secs(30 * 3_600);
        assert_eq!(absolute_label(in_30h, now, 9 * 3_600), "06-14 15:00");
    }

    #[test]
    fn civil_from_days_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // Leap day.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }
}
