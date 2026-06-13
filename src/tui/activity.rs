//! Activity log state: in-flight requests (spinner rows), a bounded ring
//! buffer of completed entries (newest first), and per-account totals.
//! Pure state — rendering lives in `ui`, timestamps are passed in.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, SystemTime};

use super::event::{ActivityEvent, TokenCounts};

/// Completed-entry ring capacity (matches teamclaude's 200-line log).
pub(crate) const LOG_CAPACITY: usize = 200;
/// In-flight rows are bounded too: if the proxy never sends a finish (bug or
/// dropped event), the oldest in-flight entry is retired as an error note
/// instead of leaking forever.
const MAX_IN_FLIGHT: usize = 64;

/// A request that has started but not finished — rendered with a spinner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InFlight {
    pub id: u64,
    pub method: String,
    pub path: String,
    pub account: Option<String>,
    pub started_at: SystemTime,
}

/// Body of a completed log entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompletedBody {
    Request {
        method: String,
        path: String,
        account: Option<String>,
        status: u16,
        duration: Duration,
        tokens: Option<TokenCounts>,
        /// Backend group ("claude"/"codex"), model slug, and reasoning effort
        /// served for this request, when known.
        group: Option<String>,
        model: Option<String>,
        effort: Option<String>,
    },
    Note {
        text: String,
        error: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Completed {
    pub at: SystemTime,
    pub body: CompletedBody,
}

/// Per-account lifetime counters for the table's totals columns and the
/// global totals pane (ok/error split + in/out token split).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Totals {
    pub requests: u64,
    /// Requests that finished with status < 400.
    pub ok: u64,
    /// Requests that finished with status >= 400.
    pub errors: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

impl Totals {
    /// Combined token count for single-number columns.
    pub(crate) fn tokens(&self) -> u64 {
        self.tokens_in.saturating_add(self.tokens_out)
    }

    fn add(&mut self, other: &Totals) {
        self.requests = self.requests.saturating_add(other.requests);
        self.ok = self.ok.saturating_add(other.ok);
        self.errors = self.errors.saturating_add(other.errors);
        self.tokens_in = self.tokens_in.saturating_add(other.tokens_in);
        self.tokens_out = self.tokens_out.saturating_add(other.tokens_out);
    }
}

#[derive(Debug, Default)]
pub(crate) struct ActivityLog {
    capacity: usize,
    in_flight: Vec<InFlight>,
    /// Front = newest (the log renders newest-top).
    completed: VecDeque<Completed>,
    totals: HashMap<String, Totals>,
    /// Requests that finished before routing (no account) — kept out of the
    /// per-account map but included in the global totals.
    unrouted: Totals,
}

impl ActivityLog {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            ..Self::default()
        }
    }

    pub(crate) fn in_flight(&self) -> &[InFlight] {
        &self.in_flight
    }

    /// Completed entries, newest first.
    pub(crate) fn completed(&self) -> impl Iterator<Item = &Completed> {
        self.completed.iter()
    }

    /// Per-account totals lookup. The dashboard reads the whole map
    /// ([`Self::totals_map`]) for the document; this single-account accessor
    /// is exercised by the unit tests.
    #[cfg(test)]
    pub(crate) fn totals_for(&self, account: &str) -> Totals {
        self.totals.get(account).copied().unwrap_or_default()
    }

    /// Clone of the per-account totals map (the dashboard document carries
    /// every account's session totals, not just the ones on screen).
    pub(crate) fn totals_map(&self) -> HashMap<String, Totals> {
        self.totals.clone()
    }

    /// Lifetime totals across every account, unrouted failures included.
    pub(crate) fn totals_global(&self) -> Totals {
        let mut sum = self.unrouted;
        for totals in self.totals.values() {
            sum.add(totals);
        }
        sum
    }

    /// Completed requests per minute over the trailing `window` (notes
    /// excluded). Bounded by the ring capacity: with the default 200-entry
    /// ring this is exact until ~200 requests land inside the window.
    pub(crate) fn requests_per_minute(&self, now: SystemTime, window: Duration) -> f64 {
        let minutes = window.as_secs_f64() / 60.0;
        if minutes <= 0.0 {
            return 0.0;
        }
        let cutoff = now.checked_sub(window);
        let count = self
            .completed
            .iter()
            .filter(|entry| matches!(entry.body, CompletedBody::Request { .. }))
            .filter(|entry| cutoff.is_none_or(|cutoff| entry.at >= cutoff))
            .count();
        count as f64 / minutes
    }

    /// Fold one proxy event into the log. `now` stamps the resulting entry.
    pub(crate) fn apply(&mut self, event: ActivityEvent, now: SystemTime) {
        match event {
            ActivityEvent::RequestStarted { id, method, path } => {
                if self.in_flight.len() >= MAX_IN_FLIGHT {
                    let lost = self.in_flight.remove(0);
                    self.push_note(
                        format!(
                            "{} {} never finished (in-flight overflow)",
                            lost.method, lost.path
                        ),
                        true,
                        now,
                    );
                }
                self.in_flight.push(InFlight {
                    id,
                    method,
                    path,
                    account: None,
                    started_at: now,
                });
            }
            ActivityEvent::RequestRouted { id, account } => {
                if let Some(entry) = self.in_flight.iter_mut().find(|r| r.id == id) {
                    entry.account = Some(account);
                }
            }
            ActivityEvent::RequestFinished {
                id,
                method,
                path,
                account,
                status,
                duration,
                tokens,
                group,
                model,
                effort,
            } => {
                let routed = self
                    .in_flight
                    .iter()
                    .position(|r| r.id == id)
                    .map(|i| self.in_flight.remove(i))
                    .and_then(|r| r.account);
                let account = account.or(routed);
                let bucket = match &account {
                    Some(name) => self.totals.entry(name.clone()).or_default(),
                    None => &mut self.unrouted,
                };
                bucket.requests += 1;
                if status < 400 {
                    bucket.ok += 1;
                } else {
                    bucket.errors += 1;
                }
                if let Some(tokens) = tokens {
                    bucket.tokens_in += tokens.input;
                    bucket.tokens_out += tokens.output;
                }
                self.push(Completed {
                    at: now,
                    body: CompletedBody::Request {
                        method,
                        path,
                        account,
                        status,
                        duration,
                        tokens,
                        group,
                        model,
                        effort,
                    },
                });
            }
            ActivityEvent::AccountSwitched { from, to, reason } => {
                let from = from.unwrap_or_else(|| "(none)".into());
                let why = reason.map(|r| format!(" ({r})")).unwrap_or_default();
                self.push_note(format!("switch {from} → {to}{why}"), false, now);
            }
            ActivityEvent::TokenRefreshed {
                account,
                expires_at_ms,
            } => {
                let expiry = std::time::UNIX_EPOCH + Duration::from_millis(expires_at_ms);
                let note = match expiry.duration_since(now) {
                    Ok(left) => format!(
                        "token refreshed: {account} (expires {})",
                        crate::scheduler::select::compact_duration(left)
                    ),
                    // Unknown (0) or already-past expiry: no suffix.
                    Err(_) => format!("token refreshed: {account}"),
                };
                self.push_note(note, false, now);
            }
            // Poller health is tracked by `App` (it feeds the poller pane,
            // not the activity list — one line per poll would drown it).
            ActivityEvent::UsagePolled { .. } => {}
            ActivityEvent::Error { context, message } => {
                let ctx = context.map(|c| format!("{c}: ")).unwrap_or_default();
                self.push_note(format!("{ctx}{message}"), true, now);
            }
        }
    }

    /// Append a TUI-internal note (reload result, switch attempt, …).
    pub(crate) fn push_note(&mut self, text: String, error: bool, now: SystemTime) {
        self.push(Completed {
            at: now,
            body: CompletedBody::Note { text, error },
        });
    }

    fn push(&mut self, entry: Completed) {
        self.completed.push_front(entry);
        self.completed.truncate(self.capacity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn note_text(entry: &Completed) -> &str {
        match &entry.body {
            CompletedBody::Note { text, .. } => text,
            other => panic!("expected note, got {other:?}"),
        }
    }

    fn started(id: u64) -> ActivityEvent {
        ActivityEvent::RequestStarted {
            id,
            method: "POST".into(),
            path: "/v1/messages".into(),
        }
    }

    fn finished(id: u64, account: Option<&str>, tokens: Option<(u64, u64)>) -> ActivityEvent {
        finished_status(id, account, tokens, 200)
    }

    fn finished_status(
        id: u64,
        account: Option<&str>,
        tokens: Option<(u64, u64)>,
        status: u16,
    ) -> ActivityEvent {
        ActivityEvent::RequestFinished {
            id,
            method: "POST".into(),
            path: "/v1/messages".into(),
            account: account.map(str::to_string),
            status,
            duration: Duration::from_millis(1_400),
            tokens: tokens.map(|(input, output)| TokenCounts { input, output }),
            group: None,
            model: None,
            effort: None,
        }
    }

    // ---- ring buffer behavior ----

    #[test]
    fn ring_buffer_evicts_oldest_and_orders_newest_first() {
        let mut log = ActivityLog::new(3);
        for i in 0..4 {
            log.push_note(format!("note-{i}"), false, at(i));
        }
        let texts: Vec<&str> = log.completed().map(note_text).collect();
        assert_eq!(
            texts,
            vec!["note-3", "note-2", "note-1"],
            "newest first, oldest evicted"
        );
    }

    #[test]
    fn capacity_is_respected_under_mixed_events() {
        let mut log = ActivityLog::new(2);
        log.apply(started(1), at(0));
        log.apply(finished(1, Some("a"), None), at(1));
        log.push_note("one".into(), false, at(2));
        log.push_note("two".into(), false, at(3));
        assert_eq!(log.completed().count(), 2);
        assert_eq!(note_text(log.completed().next().expect("entry")), "two");
    }

    // ---- request lifecycle ----

    #[test]
    fn started_request_is_in_flight_until_finished() {
        let mut log = ActivityLog::new(10);
        log.apply(started(7), at(0));
        assert_eq!(log.in_flight().len(), 1);
        assert_eq!(log.in_flight()[0].account, None);

        log.apply(
            ActivityEvent::RequestRouted {
                id: 7,
                account: "a@x.com".into(),
            },
            at(1),
        );
        assert_eq!(log.in_flight()[0].account.as_deref(), Some("a@x.com"));

        // Finish without an explicit account: the routed account is kept.
        log.apply(finished(7, None, Some((1_000, 200))), at(2));
        assert!(log.in_flight().is_empty(), "finish clears the spinner row");
        let entry = log.completed().next().expect("completed entry").clone();
        match &entry.body {
            CompletedBody::Request {
                account,
                status,
                tokens,
                ..
            } => {
                assert_eq!(account.as_deref(), Some("a@x.com"));
                assert_eq!(*status, 200);
                assert_eq!(
                    *tokens,
                    Some(TokenCounts {
                        input: 1_000,
                        output: 200,
                    })
                );
            }
            other => panic!("expected request entry, got {other:?}"),
        }
    }

    #[test]
    fn finish_without_matching_start_still_logs() {
        let mut log = ActivityLog::new(10);
        log.apply(finished(99, Some("b"), None), at(0));
        assert_eq!(log.completed().count(), 1);
        assert!(log.in_flight().is_empty());
    }

    #[test]
    fn in_flight_overflow_retires_oldest_as_error_note() {
        let mut log = ActivityLog::new(200);
        for id in 0..(MAX_IN_FLIGHT as u64 + 1) {
            log.apply(started(id), at(id));
        }
        assert_eq!(log.in_flight().len(), MAX_IN_FLIGHT);
        assert!(!log.in_flight().iter().any(|r| r.id == 0), "oldest dropped");
        let entry = log.completed().next().expect("note").clone();
        match &entry.body {
            CompletedBody::Note { error, .. } => assert!(error),
            other => panic!("expected note, got {other:?}"),
        }
    }

    // ---- totals ----

    #[test]
    fn totals_accumulate_per_account_with_ok_error_and_token_split() {
        let mut log = ActivityLog::new(10);
        log.apply(started(1), at(0));
        log.apply(finished(1, Some("a"), Some((700, 300))), at(1));
        log.apply(started(2), at(2));
        log.apply(finished(2, Some("a"), None), at(3)); // unknown tokens count 0
        log.apply(finished_status(3, Some("a"), None, 502), at(4));
        log.apply(finished(4, Some("b"), Some((20, 30))), at(5));

        assert_eq!(
            log.totals_for("a"),
            Totals {
                requests: 3,
                ok: 2,
                errors: 1,
                tokens_in: 700,
                tokens_out: 300,
            }
        );
        assert_eq!(log.totals_for("a").tokens(), 1_000);
        assert_eq!(
            log.totals_for("b"),
            Totals {
                requests: 1,
                ok: 1,
                errors: 0,
                tokens_in: 20,
                tokens_out: 30,
            }
        );
        assert_eq!(log.totals_for("ghost"), Totals::default());
    }

    #[test]
    fn unrouted_failure_counts_globally_but_not_per_account() {
        let mut log = ActivityLog::new(10);
        log.apply(started(1), at(0));
        log.apply(finished_status(1, None, None, 429), at(1)); // never routed
        log.apply(finished(2, Some("a"), Some((5, 5))), at(2));
        assert_eq!(log.totals_for("a").requests, 1);
        assert_eq!(
            log.totals_global(),
            Totals {
                requests: 2,
                ok: 1,
                errors: 1,
                tokens_in: 5,
                tokens_out: 5,
            }
        );
    }

    // ---- requests per minute ----

    #[test]
    fn rpm_counts_only_requests_inside_the_window() {
        let mut log = ActivityLog::new(50);
        let now = at(1_000);
        // 3 requests inside the 5m window, 1 outside, plus a note (ignored).
        log.apply(finished(1, Some("a"), None), at(1_000 - 400)); // outside
        log.apply(finished(2, Some("a"), None), at(1_000 - 200));
        log.apply(finished(3, Some("a"), None), at(1_000 - 100));
        log.apply(finished(4, Some("a"), None), at(1_000));
        log.push_note("switch".into(), false, at(1_000 - 50));

        let rpm = log.requests_per_minute(now, Duration::from_secs(300));
        assert!((rpm - 3.0 / 5.0).abs() < 1e-9, "got {rpm}");
    }

    #[test]
    fn rpm_zero_window_and_empty_log_are_zero() {
        let log = ActivityLog::new(10);
        assert_eq!(
            log.requests_per_minute(at(1_000), Duration::from_secs(300)),
            0.0
        );
        let mut log = ActivityLog::new(10);
        log.apply(finished(1, Some("a"), None), at(1_000));
        assert_eq!(log.requests_per_minute(at(1_000), Duration::ZERO), 0.0);
    }

    #[test]
    fn usage_polled_is_not_an_activity_line() {
        let mut log = ActivityLog::new(10);
        log.apply(
            ActivityEvent::UsagePolled {
                account: "a".into(),
                ok: true,
                consecutive_failures: 0,
                next_in: Duration::from_secs(300),
            },
            at(0),
        );
        assert_eq!(log.completed().count(), 0);
    }
}
