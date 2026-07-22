//! Event-banner time parsing for config `events` ([`crate::config::EventBanner`]).
//!
//! Pure, dependency-free (std only) so BOTH the TUI (which renders the banner)
//! and the proxy (which validates `from`/`to` on `POST /llmux/events`) can call
//! it without a UI-layer dependency inversion. Two accepted forms:
//!
//! - RFC3339 WITH an explicit offset (`2026-07-12T23:59:59-07:00`, or a `Z`
//!   suffix for UTC). Fractional seconds are tolerated and ignored.
//! - compact `YYYYMMDDHHMM` (exactly 12 ASCII digits), interpreted as LOCAL
//!   wall-clock time.
//!
//! std has no timezone database and chrono is too heavy for one offset, so the
//! local-offset lookup for the compact form reuses libc via
//! [`crate::tui::format::local_offset_secs`], while the civil-date math stays a
//! small hand-rolled, injected-offset-testable core.

use std::time::{Duration, SystemTime};

/// Parse an event timestamp (`from`/`to`) into its absolute instant.
///
/// Accepts RFC3339-with-offset (offset taken from the string) or compact
/// `YYYYMMDDHHMM` (interpreted in the machine's LOCAL zone at that instant).
/// Returns `None` on any malformed input — the banner then simply does not
/// render (never a crash), and `POST /llmux/events` rejects it with a 400.
pub fn parse_event_time(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    if is_compact(s) {
        parse_compact(s, local_offset_for_compact(s)?)
    } else {
        parse_rfc3339(s)
    }
}

/// True when `s` is exactly 12 ASCII digits — the compact `YYYYMMDDHHMM` shape.
fn is_compact(s: &str) -> bool {
    s.len() == 12 && s.bytes().all(|b| b.is_ascii_digit())
}

/// The machine's local UTC offset (seconds east of UTC) at the instant a
/// compact `YYYYMMDDHHMM` wall-clock names. The absolute instant is not yet
/// known, so the zone lookup is seeded with the provisional instant that reads
/// the wall clock as if it were UTC — accurate except within a DST-transition
/// hour, which the banner does not need to disambiguate.
fn local_offset_for_compact(s: &str) -> Option<i64> {
    let provisional = parse_compact(s, 0)?;
    Some(crate::tui::format::local_offset_secs(provisional))
}

/// Pure core: parse compact `YYYYMMDDHHMM` at an explicit local `offset_secs`
/// (seconds east of UTC) into its absolute instant. Testable without the
/// machine's zone. Seconds are always `00`. `None` on a malformed or
/// out-of-range field.
fn parse_compact(s: &str, offset_secs: i64) -> Option<SystemTime> {
    if !is_compact(s) {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: u32 = s[4..6].parse().ok()?;
    let day: u32 = s[6..8].parse().ok()?;
    let hour: u32 = s[8..10].parse().ok()?;
    let minute: u32 = s[10..12].parse().ok()?;
    civil_to_systemtime(year, month, day, hour, minute, 0, offset_secs)
}

/// Parse an RFC3339 timestamp WITH an explicit offset (or a `Z` suffix for UTC)
/// into its absolute instant. Fractional seconds are tolerated and ignored.
fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let (date, rest) = s.split_once(['T', 't'])?;
    // Separate the time-of-day from the trailing offset (`Z`, or `±HH:MM`). The
    // offset sign is the LAST '+'/'-' in the remainder — the date's dashes are
    // already split off and the time-of-day itself carries none.
    let (time, offset_secs) = match rest.strip_suffix(['Z', 'z']) {
        Some(time) => (time, 0i64),
        None => {
            let idx = rest.rfind(['+', '-'])?;
            let (time, off) = rest.split_at(idx);
            (time, parse_offset(off)?)
        }
    };

    let mut date = date.splitn(3, '-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: u32 = date.next()?.parse().ok()?;
    let day: u32 = date.next()?.parse().ok()?;

    // Drop any fractional-seconds tail before splitting on ':'.
    let mut time = time.split('.').next()?.splitn(3, ':');
    let hour: u32 = time.next()?.parse().ok()?;
    let minute: u32 = time.next()?.parse().ok()?;
    let second: u32 = time.next()?.parse().ok()?;
    civil_to_systemtime(year, month, day, hour, minute, second, offset_secs)
}

/// Civil wall-clock fields at `offset_secs` → absolute [`SystemTime`], with
/// range validation shared by both parsers. `None` on an out-of-range field;
/// a pre-epoch result clamps to the epoch (the banner only ever counts down to
/// future instants).
fn civil_to_systemtime(
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    offset_secs: i64,
) -> Option<SystemTime> {
    if month == 0 || month > 12 || day == 0 || day > 31 || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let local =
        days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    let utc = local - offset_secs;
    Some(if utc >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_secs(utc as u64)
    } else {
        SystemTime::UNIX_EPOCH
    })
}

/// Parse a `±HH:MM` (or `±HHMM`) offset into signed seconds.
fn parse_offset(s: &str) -> Option<i64> {
    let sign = match s.as_bytes().first()? {
        b'+' => 1i64,
        b'-' => -1i64,
        _ => return None,
    };
    let digits = s[1..].replace(':', "");
    if digits.len() != 4 {
        return None;
    }
    let hh: i64 = digits[0..2].parse().ok()?;
    let mm: i64 = digits[2..4].parse().ok()?;
    Some(sign * (hh * 3_600 + mm * 60))
}

/// (year, month, day) → days since the Unix epoch (Howard Hinnant's civil
/// algorithm). Kept local to this module (the TUI's `format` module and
/// `scheduler::headers` carry their own small copies of the civil math rather
/// than share a single helper) so this parser stays std-only and self-contained.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 }.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 {
        i64::from(m) - 3
    } else {
        i64::from(m) + 9
    }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- RFC3339 parsing ----

    #[test]
    fn rfc3339_offset_and_z_yield_the_same_instant() {
        // 23:59:59-07:00 is the SAME instant as 06:59:59Z the next day.
        let off = parse_event_time("2026-07-12T23:59:59-07:00").expect("offset parses");
        let z = parse_event_time("2026-07-13T06:59:59Z").expect("Z parses");
        assert_eq!(off, z);
    }

    #[test]
    fn rfc3339_rejects_garbage() {
        assert!(parse_event_time("not-a-timestamp").is_none());
        assert!(parse_event_time("2026-13-40T99:00:00Z").is_none());
    }

    // ---- compact `YYYYMMDDHHMM` parsing (injected offset) ----

    #[test]
    fn compact_local_matches_rfc3339_with_the_same_offset() {
        // 202607122359 read at -07:00 == 2026-07-12T23:59:00-07:00.
        let compact = parse_compact("202607122359", -7 * 3_600).expect("compact parses");
        let rfc = parse_rfc3339("2026-07-12T23:59:00-07:00").expect("rfc3339 parses");
        assert_eq!(compact, rfc);
        // A different injected offset shifts the instant by exactly that delta:
        // the same wall clock at -07:00 (west of UTC) is a LATER instant than at
        // UTC by 7h (UTC = local + 7h).
        let utc = parse_compact("202607122359", 0).expect("compact utc");
        assert_eq!(
            compact.duration_since(utc).expect("west offset is later"),
            Duration::from_secs(7 * 3_600),
        );
    }

    #[test]
    fn compact_rejects_wrong_length_and_bad_fields() {
        assert!(parse_compact("20260712235", 0).is_none(), "11 digits");
        assert!(parse_compact("2026071223590", 0).is_none(), "13 digits");
        assert!(parse_compact("2026071223xx", 0).is_none(), "non-digit");
        assert!(parse_compact("202613122359", 0).is_none(), "month 13");
        assert!(parse_compact("202607122400", 0).is_none(), "hour 24");
    }

    #[test]
    fn parse_event_time_accepts_both_forms() {
        // The public entry point routes 12-digit strings to the compact parser
        // and everything else to RFC3339; both return Some for valid input.
        assert!(parse_event_time("202607080000").is_some(), "compact");
        assert!(
            parse_event_time("2026-07-08T00:00:00-07:00").is_some(),
            "rfc3339"
        );
    }
}
