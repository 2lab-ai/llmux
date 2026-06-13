//! `ActivityEvent` — the proxy→TUI activity feed contract.
//!
//! The TUI owns this type (re-exported as `crate::tui::ActivityEvent`); the
//! proxy holds the `tokio::sync::mpsc::Sender<ActivityEvent>` side and emits
//! one event per observable happening. Senders should use `try_send` and drop
//! the event on a full channel — the dashboard is best-effort observability
//! and must never backpressure the request path.

use std::time::Duration;

/// Input/output token counts for one completed request, when the upstream
/// response carried usage. Split so the dashboard can show in/out totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
}

impl TokenCounts {
    /// Combined count for single-number displays.
    pub fn total(self) -> u64 {
        self.input.saturating_add(self.output)
    }
}

/// One observable proxy/scheduler happening, rendered into the activity log.
///
/// `id` correlates the started → routed → finished lifecycle of a single
/// request (any per-process unique counter works; it never leaves the TUI).
/// `RequestFinished` repeats `method`/`path`/`account` so a finish whose
/// start was dropped (channel full, TUI attached late) still renders as a
/// complete log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityEvent {
    /// A client request was accepted; shows as in-flight (spinner) until the
    /// matching `RequestFinished` arrives.
    RequestStarted {
        id: u64,
        method: String,
        path: String,
    },
    /// The scheduler leased an account for request `id`.
    RequestRouted { id: u64, account: String },
    /// Request `id` completed (any status, including upstream errors).
    RequestFinished {
        id: u64,
        method: String,
        path: String,
        /// Account that served it; `None` if it failed before routing.
        account: Option<String>,
        /// HTTP status returned to the client.
        status: u16,
        duration: Duration,
        /// Tokens extracted from the response usage, when available — feeds
        /// the per-account in/out token totals.
        tokens: Option<TokenCounts>,
    },
    /// The scheduler committed a switch of the current account.
    AccountSwitched {
        /// `None` on the initial selection.
        from: Option<String>,
        to: String,
        /// Human-readable cause ("429 park", "5h threshold", …).
        reason: Option<String>,
    },
    /// An OAuth access token was refreshed for `account`.
    TokenRefreshed {
        account: String,
        /// New access-token expiry, epoch ms — lets the activity line show
        /// how much lifetime the refresh bought.
        expires_at_ms: u64,
    },
    /// The usage poller finished one poll attempt for `account` — feeds the
    /// dashboard's poller-health pane (last success age, backoff, next ETA).
    UsagePolled {
        account: String,
        /// Whether this attempt succeeded.
        ok: bool,
        /// Consecutive failures so far (0 after a success).
        consecutive_failures: u32,
        /// Delay until the next scheduled poll of this account.
        next_in: Duration,
    },
    /// Anything that went wrong and deserves operator eyes.
    Error {
        /// What was being attempted ("usage poll", "refresh", …).
        context: Option<String>,
        message: String,
    },
}
