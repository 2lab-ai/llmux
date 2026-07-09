//! Event-banner deadline parsing for the config `event` block ([`crate::config::EventConfig`]).
//!
//! Pure, dependency-free (std only) so BOTH the TUI (which renders the banner)
//! and the proxy (which validates `event.until` on `POST /llmux/event`) can call
//! it without a UI-layer dependency inversion. std has no timezone database and
//! chrono is too heavy for one offset, so this is a small hand-rolled RFC3339
//! parser paired with Howard Hinnant's civil-date math.

use std::time::{Duration, SystemTime};

/// A parsed event deadline (config `event.until`): the absolute instant plus
/// the civil wall-clock fields in the timestamp's OWN offset, so the banner can
/// render the deadline in the zone the operator wrote it in.
pub struct EventDeadline {
    /// Absolute deadline instant (UTC), for the live countdown vs. `now`.
    pub at: SystemTime,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    offset_secs: i64,
}

impl EventDeadline {
    /// The deadline in its own offset: `7/12 23:59:59 PT` — month/day unpadded,
    /// wall-clock zero-padded, zone labeled from the offset.
    pub fn display_label(&self) -> String {
        format!(
            "{}/{} {:02}:{:02}:{:02} {}",
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second,
            zone_label(self.offset_secs),
        )
    }
}

/// Parse an RFC3339 timestamp WITH an explicit offset (`2026-07-12T23:59:59-07:00`,
/// or a `Z` suffix for UTC) into an [`EventDeadline`]. Fractional seconds are
/// tolerated and ignored. Returns `None` on any malformed input — the banner
/// then simply does not render (never a crash), and the `POST /llmux/event`
/// endpoint rejects it with a 400.
pub fn parse_event_deadline(s: &str) -> Option<EventDeadline> {
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
    if month == 0 || month > 12 || day == 0 || day > 31 || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let local =
        days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    let utc = local - offset_secs;
    let at = if utc >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_secs(utc as u64)
    } else {
        SystemTime::UNIX_EPOCH
    };
    Some(EventDeadline {
        at,
        month,
        day,
        hour,
        minute,
        second,
        offset_secs,
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

/// Zone label for a UTC offset in seconds: the two North-American zones the
/// event banner special-cases (`-07:00`→`PT`, `-04:00`→`ET`), else a generic
/// `UTC±HH` (`+09:00`→`UTC+09`) — with `:MM` appended for a sub-hour offset.
pub fn zone_label(offset_secs: i64) -> String {
    match offset_secs {
        -25_200 => "PT".to_string(),
        -14_400 => "ET".to_string(),
        0 => "UTC".to_string(),
        _ => {
            let sign = if offset_secs < 0 { '-' } else { '+' };
            let abs = offset_secs.abs();
            let (hh, mm) = (abs / 3_600, (abs % 3_600) / 60);
            if mm == 0 {
                format!("UTC{sign}{hh:02}")
            } else {
                format!("UTC{sign}{hh:02}:{mm:02}")
            }
        }
    }
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

    // ---- offset → zone label ----

    #[test]
    fn zone_label_maps_offsets() {
        assert_eq!(zone_label(-7 * 3_600), "PT", "-07:00 → PT");
        assert_eq!(zone_label(-4 * 3_600), "ET", "-04:00 → ET");
        assert_eq!(zone_label(0), "UTC");
        // Generic fallback: UTC±HH, zero-padded hours.
        assert_eq!(zone_label(9 * 3_600), "UTC+09", "+09:00 → UTC+09");
        assert_eq!(zone_label(-3 * 3_600), "UTC-03", "-03:00 → UTC-03");
        // Sub-hour offset keeps its minutes.
        assert_eq!(zone_label(5 * 3_600 + 30 * 60), "UTC+05:30");
    }

    // ---- RFC3339 deadline parsing ----

    #[test]
    fn parse_event_deadline_renders_in_its_own_offset() {
        let d = parse_event_deadline("2026-07-12T23:59:59-07:00").expect("parse");
        // The wall-clock label is the timestamp's OWN offset, zone from it.
        assert_eq!(d.display_label(), "7/12 23:59:59 PT");
        // The absolute instant is offset-adjusted: 23:59:59-07:00 is the SAME
        // instant as 06:59:59Z the next day — parsing either yields it.
        let same = parse_event_deadline("2026-07-13T06:59:59Z").expect("parse Z");
        assert_eq!(d.at, same.at);
    }

    #[test]
    fn parse_event_deadline_accepts_z_and_rejects_garbage() {
        let z = parse_event_deadline("2026-01-01T00:00:00Z").expect("Z parses");
        assert_eq!(z.display_label(), "1/1 00:00:00 UTC");
        assert!(parse_event_deadline("not-a-timestamp").is_none());
        assert!(parse_event_deadline("2026-13-40T99:00:00Z").is_none());
    }
}
