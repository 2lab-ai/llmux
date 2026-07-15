//! Activity log state: in-flight requests (spinner rows), a bounded ring
//! buffer of completed entries (newest first), and per-account totals.
//! Pure state — rendering lives in `ui`, timestamps are passed in.

use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::event::{ActivityEvent, TokenCounts};

/// Completed-entry ring capacity (matches teamclaude's 200-line log).
pub(crate) const LOG_CAPACITY: usize = 200;
/// Distinct-client cap for per-client request attribution (issue #32). The
/// `metadata.user_id` space is operator/agent controlled and small in practice,
/// but a hostile or buggy client could send a fresh id per request; this bounds
/// the in-memory map so it can never grow unbounded. When the cap is reached, a
/// *new* client id is folded into the shared `unknown` bucket instead of
/// allocating a new entry (existing ids keep accumulating). The `unknown`
/// bucket itself never counts against the cap.
pub(crate) const MAX_CLIENTS: usize = 1024;
/// The bucket name for requests with no `metadata.user_id` (issue #32). These
/// are attributed here, never dropped.
pub(crate) const UNKNOWN_CLIENT: &str = "unknown";
/// In-flight rows are bounded too: if the proxy never sends a finish (bug or
/// dropped event), the oldest in-flight entry is retired as an error note
/// instead of leaking forever.
const MAX_IN_FLIGHT: usize = 64;
/// Rolling window the header health verdict aggregates over (glance-triage).
pub(crate) const HEALTH_WINDOW: Duration = Duration::from_secs(300);
/// One bucket per second of [`HEALTH_WINDOW`] (+1 for the partial current
/// second): the aggregation is EXACT for any request rate at fixed memory —
/// a raw per-event deque with a length cap would silently shorten the time
/// window during a storm, exactly when accuracy matters.
const HEALTH_BUCKET_CAP: usize = HEALTH_WINDOW.as_secs() as usize + 1;
/// Age after which an in-flight row is presumed finished and swept, even if no
/// `RequestFinished` event ever arrived (the event was dropped on a full
/// activity channel). Real requests finish in well under 90s per the daemon
/// logs, so 300s is a wide safety margin that never retires a live request but
/// still bounds a leaked row's lifetime — instead of growing to 25,000s+.
const STALE_IN_FLIGHT: Duration = Duration::from_secs(300);

/// A request that has started but not finished — rendered with a spinner.
/// `group`/`model`/`effort`/`fast` are filled at routing time so the dashboard
/// can attribute in-flight requests to a model row — and show the same
/// metadata badge as completed rows — before they complete (req11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InFlight {
    pub id: u64,
    pub method: String,
    pub path: String,
    pub account: Option<String>,
    pub group: Option<String>,
    pub model: Option<String>,
    /// Per-request effective reasoning effort, when known at routing time.
    pub effort: Option<String>,
    /// Codex fast mode in effect (always `false` for claude).
    pub fast: bool,
    pub started_at: SystemTime,
}

/// Body of a completed log entry.
// Request dwarfs Note by design: nearly every entry IS a Request (Notes are
// rare operator lines), so boxing the common variant would trade one heap
// allocation per real entry for slack in the rare one.
#[allow(clippy::large_enum_variant)]
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
        /// Codex fast mode was in effect (always `false` for claude).
        fast: bool,
        /// Keyless client identity (`metadata.user_id`) — keys the derived
        /// session label shown on the row (TUI UI-3 U2).
        user_id: Option<String>,
        /// Message-kind token from `proxy::classify` ("user"/"compact"/…).
        kind: Option<String>,
        /// Cleaned input excerpt (bounded), shown truncated on the row and
        /// in full on the click-expanded detail line.
        excerpt: Option<String>,
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

/// A STABLE identity for a completed *request* entry, used by the TUI to track
/// which activity row is click-expanded across redraws (Feature B). The
/// completed-entry body carries no request `id` (it is dropped at finish), so
/// the key is the tuple that survives new rows prepending: completion time
/// (epoch ms) + method + path + status. A list index would NOT survive (new
/// rows shift everything down), so we never key on position. `Note` entries are
/// not expandable and have no key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ActivityKey {
    pub at_ms: u64,
    pub method: String,
    pub path: String,
    pub status: u16,
}

impl Completed {
    /// Stable expand-identity for this entry, or `None` when it is a `Note`
    /// (notes are never expandable — they carry no request detail).
    pub(crate) fn activity_key(&self) -> Option<ActivityKey> {
        match &self.body {
            CompletedBody::Request {
                method,
                path,
                status,
                ..
            } => Some(ActivityKey {
                at_ms: self
                    .at
                    .duration_since(UNIX_EPOCH)
                    .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
                    .unwrap_or(0),
                method: method.clone(),
                path: path.clone(),
                status: *status,
            }),
            CompletedBody::Note { .. } => None,
        }
    }
}

/// Days of per-day/per-model token history retained for the Tokens-per-Day
/// chart (UI-3 U14).
const DAILY_RETAIN_DAYS: u64 = 90;

/// One (day, group, model) cell of the Tokens-per-Day chart (UI-3 U14).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DailyTokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

/// Calendar granularities of the Usage tab (usage-stats): hourly, daily,
/// monthly buckets over the persisted request history. Distinct from
/// [`StatsWindow`] (trailing 24h/72h windows) — these are CALENDAR buckets
/// the operator reads like a bill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum UsageGran {
    Hour,
    #[default]
    Day,
    Month,
}

impl UsageGran {
    /// The next granularity in the `g` cycle (hour → day → month → hour).
    pub(crate) fn next(self) -> UsageGran {
        match self {
            UsageGran::Hour => UsageGran::Day,
            UsageGran::Day => UsageGran::Month,
            UsageGran::Month => UsageGran::Hour,
        }
    }

    /// Wire tag carried on [`crate::dashboard::UsageStatDoc::gran`]. Stable —
    /// the attach client matches on it.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            UsageGran::Hour => "hour",
            UsageGran::Day => "day",
            UsageGran::Month => "month",
        }
    }

    /// UI label for the Usage tab header.
    pub(crate) fn label(self) -> &'static str {
        match self {
            UsageGran::Hour => "hourly",
            UsageGran::Day => "daily",
            UsageGran::Month => "monthly",
        }
    }
}

/// Hourly usage buckets retained for the Usage tab (72h — matches the widest
/// [`StatsWindow`]). Older hours are answered by the daily/monthly rollups;
/// arbitrary-past hourly drill-down needs the paged store (issue #107).
const USAGE_HOURLY_RETAIN_HOURS: u64 = 72;
/// Daily usage buckets retained for the Usage tab. Wider than the chart's
/// [`DAILY_RETAIN_DAYS`] so the calendar table reaches back two quarters;
/// months beyond this are still covered by the unbounded monthly rollup.
const USAGE_DAILY_RETAIN_DAYS: u64 = 180;

/// One (bucket, group, model) cell of the Usage tab: request count + the four
/// token counters. Unlike [`DailyTokens`] (chart cells, token events only),
/// a cell counts EVERY attributed finished request — a failed request with no
/// usage block still shows up in the requests column, mirroring the
/// per-model row semantics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct UsageCell {
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl UsageCell {
    /// Fold one finished request into the cell.
    fn add(&mut self, tokens: Option<TokenCounts>) {
        self.requests = self.requests.saturating_add(1);
        if let Some(t) = tokens {
            self.input = self.input.saturating_add(t.input);
            self.output = self.output.saturating_add(t.output);
            self.cache_read = self.cache_read.saturating_add(t.cache_read.unwrap_or(0));
            self.cache_creation = self
                .cache_creation
                .saturating_add(t.cache_creation.unwrap_or(0));
        }
    }

    /// Merge another cell (history-behind hydration).
    fn merge(&mut self, other: &UsageCell) {
        self.requests = self.requests.saturating_add(other.requests);
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_creation = self.cache_creation.saturating_add(other.cache_creation);
    }
}

/// One usage-bucket map: bucket key → (group, model) → cell. Hour keys are
/// epoch HOURS (UTC); day keys are LOCAL civil days (epoch days of the
/// offset-shifted clock); month keys are LOCAL civil `year*12 + month0`.
type UsageBuckets = std::collections::BTreeMap<u64, HashMap<(String, String), UsageCell>>;

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

// ---------------------------------------------------------------------------
// Model-usage aggregation (req1-20): per (group, served_model) row.
// ---------------------------------------------------------------------------

/// In-memory accumulator for one model row. Folded from completed request
/// events; reset on daemon restart (runtime-only, req26). Cache counters are
/// optional — `None` until an upstream reports the field (req8/9).
#[derive(Debug, Default, Clone)]
struct ModelStats {
    requests: u64,
    ok: u64,
    errors: u64,
    tokens_in: u64,
    tokens_out: u64,
    cache_read: Option<u64>,
    cache_creation: Option<u64>,
    last_used: Option<SystemTime>,
    /// Which account(s) served this model (req19).
    accounts: HashMap<String, Totals>,
    /// Reasoning/effort label → request count (req18); `"none"` when unset.
    efforts: HashMap<String, u64>,
    /// Endpoint class → request count (req20): `messages`/`count_tokens`/other.
    endpoints: HashMap<String, u64>,
}

impl ModelStats {
    /// Fold another row's counters into this one (background history
    /// hydration): every counter sums; `last_used` keeps the later of the two
    /// (history is older, so a live row's timestamp survives the merge).
    fn absorb(&mut self, other: ModelStats) {
        self.requests = self.requests.saturating_add(other.requests);
        self.ok = self.ok.saturating_add(other.ok);
        self.errors = self.errors.saturating_add(other.errors);
        self.tokens_in = self.tokens_in.saturating_add(other.tokens_in);
        self.tokens_out = self.tokens_out.saturating_add(other.tokens_out);
        self.cache_read = crate::proxy::sse::add_opt(self.cache_read, other.cache_read);
        self.cache_creation = crate::proxy::sse::add_opt(self.cache_creation, other.cache_creation);
        self.last_used = match (self.last_used, other.last_used) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        for (name, totals) in other.accounts {
            self.accounts.entry(name).or_default().add(&totals);
        }
        for (label, count) in other.efforts {
            let entry = self.efforts.entry(label).or_default();
            *entry = entry.saturating_add(count);
        }
        for (label, count) in other.endpoints {
            let entry = self.endpoints.entry(label).or_default();
            *entry = entry.saturating_add(count);
        }
    }
}

/// A finished aggregated model row (snapshot of [`ModelStats`]). Timestamps are
/// kept as `SystemTime`; the document builder converts to epoch ms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelUsage {
    pub group: String,
    pub model: String,
    pub requests: u64,
    pub ok: u64,
    pub errors: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read: Option<u64>,
    pub cache_creation: Option<u64>,
    pub last_used: SystemTime,
    pub accounts: Vec<ModelAccount>,
    pub efforts: Vec<ModelCount>,
    pub endpoints: Vec<ModelCount>,
}

/// Per-account contribution to one model row (req19).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelAccount {
    pub name: String,
    pub requests: u64,
    pub ok: u64,
    pub errors: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

/// A labelled request count (effort level or endpoint class).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelCount {
    pub label: String,
    pub requests: u64,
}

/// Strip a trailing display-only context suffix `…[1m]` so usage is not split
/// by client window hints (req17): `claude-sonnet-4-5[1m]` → `claude-sonnet-4-5`.
pub(crate) fn normalize_model(model: &str) -> String {
    match model.split_once('[') {
        Some((base, _)) => base.trim().to_string(),
        None => model.trim().to_string(),
    }
}

/// Classify a request path into an endpoint bucket for the per-model breakdown
/// (req20). `count_tokens` is checked first because its path also contains
/// `/messages`.
fn endpoint_class(path: &str) -> String {
    let p = path.split('?').next().unwrap_or(path);
    if p.contains("count_tokens") {
        "count_tokens".to_string()
    } else if p.contains("/messages") {
        "messages".to_string()
    } else {
        p.rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or("other")
            .to_string()
    }
}

fn sorted_counts(map: &HashMap<String, u64>) -> Vec<ModelCount> {
    let mut counts: Vec<ModelCount> = map
        .iter()
        .map(|(label, &requests)| ModelCount {
            label: label.clone(),
            requests,
        })
        .collect();
    counts.sort_by(|a, b| b.requests.cmp(&a.requests).then(a.label.cmp(&b.label)));
    counts
}

// ---------------------------------------------------------------------------
// Windowed bucket ring (issue #23): rolling hourly counters keyed by
// (group, normalized_model, account), so 24h / 72h per-account/per-model
// heatmaps are computable IN MEMORY (no durable store — that is a follow-up).
//
// Each bucket covers one wall-clock hour (epoch-hour index = secs / 3600). The
// ring keeps [`BUCKET_COUNT`] hours; folding a request rolls the ring forward
// to the current hour and PRUNES expired buckets entirely (not just zeroes
// them) so stray/typo model keys can never grow unbounded. SystemTime is not
// monotonic — every hour computation clamps on skew rather than panicking.
// ---------------------------------------------------------------------------

/// Seconds per bucket — one wall-clock hour.
const BUCKET_SECS: u64 = 3600;
/// How many hourly buckets the ring retains. 73 covers a full 72h window plus
/// the current partial hour, so a 72h view never loses a still-relevant hour
/// to roll-forward before the window itself expires it.
const BUCKET_COUNT: usize = 73;

/// The windows the heatmap surfaces. Kept small and fixed (24h, 72h) — both fit
/// inside the retained ring, so each is exact up to the lossy event channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum StatsWindow {
    #[default]
    Day,
    ThreeDay,
}

impl StatsWindow {
    /// All windows the dashboard renders, narrowest first.
    pub(crate) const ALL: [StatsWindow; 2] = [StatsWindow::Day, StatsWindow::ThreeDay];

    /// The next window in the cycle (24h ↔ 72h), for the `w` toggle.
    pub(crate) fn next(self) -> StatsWindow {
        match self {
            StatsWindow::Day => StatsWindow::ThreeDay,
            StatsWindow::ThreeDay => StatsWindow::Day,
        }
    }

    /// Trailing duration this window aggregates over.
    pub(crate) fn duration(self) -> Duration {
        match self {
            StatsWindow::Day => Duration::from_secs(24 * 3600),
            StatsWindow::ThreeDay => Duration::from_secs(72 * 3600),
        }
    }

    /// Short label for the UI ("24h" / "72h").
    pub(crate) fn label(self) -> &'static str {
        match self {
            StatsWindow::Day => "24h",
            StatsWindow::ThreeDay => "72h",
        }
    }
}

/// Per-bucket counters for one `(group, model, account)` key. Mirrors the
/// cumulative `ModelStats` fields the issue calls for, but scoped to one hour.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WindowCounts {
    pub requests: u64,
    pub ok: u64,
    pub errors: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl WindowCounts {
    fn add(&mut self, other: &WindowCounts) {
        self.requests = self.requests.saturating_add(other.requests);
        self.ok = self.ok.saturating_add(other.ok);
        self.errors = self.errors.saturating_add(other.errors);
        self.tokens_in = self.tokens_in.saturating_add(other.tokens_in);
        self.tokens_out = self.tokens_out.saturating_add(other.tokens_out);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_creation = self.cache_creation.saturating_add(other.cache_creation);
    }

    /// Combined token count for the heatmap intensity (in + out + cache).
    pub(crate) fn tokens(&self) -> u64 {
        self.tokens_in
            .saturating_add(self.tokens_out)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_creation)
    }
}

/// The bucket key. Carries `group` AND `model` AND `account` so a same-named
/// model under two providers stays two rows and the per-account axis exists.
type WindowKey = (String, String, String);

/// One hour's counters: the epoch-hour index it represents + the per-key map.
#[derive(Debug, Default, Clone)]
struct Bucket {
    /// `epoch_secs / BUCKET_SECS`. A bucket is "current" when this equals the
    /// hour derived from `now`.
    hour: u64,
    counts: HashMap<WindowKey, WindowCounts>,
}

/// A fixed-capacity ring of hourly buckets. Folding is O(1) amortized: roll
/// forward to the current hour (reusing slots, clearing stale ones) then bump
/// the one current bucket's key.
#[derive(Debug, Default)]
struct WindowedBuckets {
    buckets: VecDeque<Bucket>,
}

/// Epoch-hour index for `now`, clamped on a pre-epoch clock (skew defence —
/// never panics).
fn epoch_hour(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / BUCKET_SECS)
        .unwrap_or(0)
}

impl WindowedBuckets {
    /// Roll the ring forward so its newest bucket is `current_hour`, dropping
    /// (pruning, not zeroing) buckets that fall out of the retained range. A
    /// backwards clock (`current_hour` older than the newest) is ignored — we
    /// never rewind, so skew can't corrupt or panic the ring.
    fn roll_to(&mut self, current_hour: u64) {
        match self.buckets.back() {
            Some(newest) if current_hour <= newest.hour => return,
            _ => {}
        }
        // Append the current hour. If the ring already has buckets and there is
        // a gap (idle hours), we do NOT materialize the empty intermediate
        // hours — pruning by `hour` value at read time handles the window math,
        // and a single appended current bucket keeps roll-forward O(1).
        self.buckets.push_back(Bucket {
            hour: current_hour,
            counts: HashMap::new(),
        });
        // Prune anything older than the retained range AND cap the deque length
        // (an idle-then-active daemon can leave sparse old buckets).
        let oldest_kept = current_hour.saturating_sub(BUCKET_COUNT as u64 - 1);
        self.buckets.retain(|b| b.hour >= oldest_kept);
        while self.buckets.len() > BUCKET_COUNT {
            self.buckets.pop_front();
        }
    }

    /// Fold one finished, attributed request into the current bucket.
    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        group: &str,
        model: &str,
        account: &str,
        status: u16,
        tokens: Option<TokenCounts>,
        now: SystemTime,
    ) {
        let hour = epoch_hour(now);
        self.roll_to(hour);
        // After roll_to, the matching current bucket is the newest with
        // `hour == current`; if a backwards clock skipped the append, fold into
        // the newest bucket we have rather than dropping the event.
        let bucket = match self.buckets.back_mut() {
            Some(b) => b,
            None => {
                self.buckets.push_back(Bucket {
                    hour,
                    counts: HashMap::new(),
                });
                self.buckets.back_mut().expect("just pushed")
            }
        };
        let key = (
            group.to_string(),
            normalize_model(model),
            account.to_string(),
        );
        let entry = bucket.counts.entry(key).or_default();
        entry.requests = entry.requests.saturating_add(1);
        if status < 400 {
            entry.ok = entry.ok.saturating_add(1);
        } else {
            entry.errors = entry.errors.saturating_add(1);
        }
        if let Some(t) = tokens {
            entry.tokens_in = entry.tokens_in.saturating_add(t.input);
            entry.tokens_out = entry.tokens_out.saturating_add(t.output);
            entry.cache_read = entry.cache_read.saturating_add(t.cache_read.unwrap_or(0));
            entry.cache_creation = entry
                .cache_creation
                .saturating_add(t.cache_creation.unwrap_or(0));
        }
    }

    /// Merge another ring's buckets into this one BY EPOCH HOUR (background
    /// history hydration). Each historical bucket lands in the hour it
    /// originally covered — never the current hour — so the 24h/72h heatmaps
    /// read the same as if history had been replayed before live traffic.
    /// Rebuilds the deque hour-ascending, pruned to the retained range of the
    /// newest hour present and capped at [`BUCKET_COUNT`], exactly the
    /// invariant [`Self::roll_to`] maintains.
    fn merge_behind(&mut self, other: WindowedBuckets) {
        if other.buckets.is_empty() {
            return;
        }
        let mut by_hour: std::collections::BTreeMap<u64, HashMap<WindowKey, WindowCounts>> =
            std::collections::BTreeMap::new();
        for bucket in self.buckets.drain(..).chain(other.buckets) {
            let merged = by_hour.entry(bucket.hour).or_default();
            for (key, counts) in bucket.counts {
                merged.entry(key).or_default().add(&counts);
            }
        }
        let Some(&newest) = by_hour.keys().next_back() else {
            return;
        };
        let oldest_kept = newest.saturating_sub(BUCKET_COUNT as u64 - 1);
        self.buckets = by_hour
            .into_iter()
            .filter(|&(hour, _)| hour >= oldest_kept)
            .map(|(hour, counts)| Bucket { hour, counts })
            .collect();
        while self.buckets.len() > BUCKET_COUNT {
            self.buckets.pop_front();
        }
    }

    /// Aggregate every key over the trailing `window` ending at `now`, summing
    /// the buckets whose hour falls inside it. Returns one [`WindowedRow`] per
    /// `(group, model, account)` with any activity in the window.
    fn aggregate(&self, window: StatsWindow, now: SystemTime) -> Vec<WindowedRow> {
        let current_hour = epoch_hour(now);
        // Number of whole hours the window spans; the trailing bucket is the
        // current hour, so a 24h window includes the current hour + 23 prior.
        let span_hours = (window.duration().as_secs() / BUCKET_SECS).max(1);
        let cutoff_hour = current_hour.saturating_sub(span_hours - 1);
        let mut acc: HashMap<WindowKey, WindowCounts> = HashMap::new();
        for bucket in &self.buckets {
            if bucket.hour < cutoff_hour || bucket.hour > current_hour {
                continue;
            }
            for (key, counts) in &bucket.counts {
                acc.entry(key.clone()).or_default().add(counts);
            }
        }
        let mut rows: Vec<WindowedRow> = acc
            .into_iter()
            .map(|((group, model, account), counts)| WindowedRow {
                group,
                model,
                account,
                counts,
            })
            .collect();
        // Deterministic order: tokens desc, then key — the heatmap reads top-down.
        rows.sort_by(|a, b| {
            b.counts
                .tokens()
                .cmp(&a.counts.tokens())
                .then(b.counts.requests.cmp(&a.counts.requests))
                .then(a.group.cmp(&b.group))
                .then(a.model.cmp(&b.model))
                .then(a.account.cmp(&b.account))
        });
        rows
    }
}

/// One aggregated windowed cell: a `(group, model, account)` triple and its
/// summed counters over a window. The heatmap renders one of these per visible
/// cell; the document carries the full set per window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowedRow {
    pub group: String,
    pub model: String,
    pub account: String,
    pub counts: WindowCounts,
}

// ---------------------------------------------------------------------------
// Persistence (req-persist): append-only JSONL of finished requests.
//
// Two user requirements satisfied by ONE store: (A) model/account stats survive
// restart and continue cumulatively, and (C) activity request/response records
// are persisted with no retention limit. The single source of truth is one
// JSON line per `RequestFinished`, replayed on startup through the SAME `apply`
// fold so the rebuilt aggregates are bit-for-bit identical to the live ones —
// no double-counting. Mirrors `proxy::codex_trace`: best-effort append, every
// IO/serde error swallowed, the request path is NEVER affected.
// ---------------------------------------------------------------------------

/// On-disk schema version for [`PersistedRequest`]. Bumped only on a
/// breaking layout change; older/garbage lines are skipped on load, never
/// fatal.
const PERSIST_VERSION: u8 = 1;

/// One finished request, serialized as a single JSON line. Carries exactly the
/// fields of an [`ActivityEvent::RequestFinished`] needed to reconstruct it for
/// replay (`Duration` flattened to `duration_ms`, `SystemTime` to `ts_ms` since
/// the Unix epoch). Field-named JSON so adding a field stays backward-readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistedRequest {
    /// Schema version (`= PERSIST_VERSION`). Lines with an unknown version are
    /// skipped on load.
    pub v: u8,
    /// Completion timestamp, millis since the Unix epoch.
    pub ts_ms: u64,
    pub id: u64,
    pub method: String,
    pub path: String,
    pub account: Option<String>,
    pub status: u16,
    pub duration_ms: u64,
    pub tokens: Option<TokenCounts>,
    pub group: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Codex fast mode was in effect (always `false` for claude). Additive:
    /// lines persisted before this field default to `false`.
    #[serde(default)]
    pub fast: bool,
    /// Keyless per-client metering identity (issue #32). Additive: lines
    /// persisted before this field default to `None` and replay into the
    /// `unknown` client bucket.
    #[serde(default)]
    pub user_id: Option<String>,
    /// Message kind + input excerpt (TUI UI-3 U1). Additive: lines persisted
    /// before these fields default to `None`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub excerpt: Option<String>,
}

impl PersistedRequest {
    /// Build a record from a `RequestFinished` event's fields + the `now` it was
    /// folded at. Returns `None` for any other event variant (only finished
    /// requests are persisted — notes/switches/polls are runtime-only).
    pub(crate) fn from_event(event: &ActivityEvent, now: SystemTime) -> Option<Self> {
        let ActivityEvent::RequestFinished {
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
            fast,
            user_id,
            kind,
            excerpt,
        } = event
        else {
            return None;
        };
        let ts_ms = now
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        Some(Self {
            v: PERSIST_VERSION,
            ts_ms,
            id: *id,
            method: method.clone(),
            path: path.clone(),
            account: account.clone(),
            status: *status,
            duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            tokens: *tokens,
            group: group.clone(),
            model: model.clone(),
            effort: effort.clone(),
            fast: *fast,
            user_id: user_id.clone(),
            kind: kind.clone(),
            excerpt: excerpt.clone(),
        })
    }

    /// Reconstruct the `(event, ts)` pair this record was built from, so replay
    /// can fold it through `ActivityLog::apply` exactly as the live event was.
    fn into_event(self) -> (ActivityEvent, SystemTime) {
        let ts = UNIX_EPOCH + Duration::from_millis(self.ts_ms);
        let event = ActivityEvent::RequestFinished {
            id: self.id,
            method: self.method,
            path: self.path,
            account: self.account,
            status: self.status,
            duration: Duration::from_millis(self.duration_ms),
            tokens: self.tokens,
            group: self.group,
            model: self.model,
            effort: self.effort,
            fast: self.fast,
            user_id: self.user_id,
            kind: self.kind,
            excerpt: self.excerpt,
        };
        (event, ts)
    }
}

/// Append one finished-request record to `path` as a single JSON line,
/// best-effort. A `None` path (no state dir), a non-`RequestFinished` event, or
/// any IO/serde error is swallowed — exactly like [`crate::proxy::codex_trace`]
/// — so persistence can never break or slow the request path. The parent dir is
/// created if missing; the file is opened `create(true).append(true)`.
pub(crate) fn persist_request(path: Option<&Path>, event: &ActivityEvent, now: SystemTime) {
    let Some(path) = path else {
        return;
    };
    let Some(record) = PersistedRequest::from_event(event, now) else {
        return;
    };
    let Ok(line) = serde_json::to_string(&record) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let _ = writeln!(file, "{line}");
}

#[derive(Debug, Default)]
pub(crate) struct ActivityLog {
    capacity: usize,
    in_flight: Vec<InFlight>,
    /// Derived session titles (TUI UI-3 U2): client `user_id` → the first
    /// plain user-input excerpt seen for it (≤48 chars). Insert-only, bounded
    /// by [`MAX_CLIENTS`].
    session_labels: HashMap<String, String>,
    /// Tokens-per-day chart data (UI-3 U14): day (epoch days) → (group,
    /// model) → summed token counts. Fed by the same RequestFinished fold
    /// (startup replay of the persisted request log fills history), pruned to
    /// [`DAILY_RETAIN_DAYS`].
    daily: std::collections::BTreeMap<u64, HashMap<(String, String), DailyTokens>>,
    /// Calendar-bucketed usage for the Usage tab (usage-stats): epoch-hour /
    /// local-civil-day / local-civil-month keys → (group, model) → cell. Fed
    /// by the same RequestFinished fold (startup replay fills history), pruned
    /// to [`USAGE_HOURLY_RETAIN_HOURS`] / [`USAGE_DAILY_RETAIN_DAYS`]; the
    /// monthly rollup is unbounded (12 keys/year).
    usage_hourly: UsageBuckets,
    usage_daily: UsageBuckets,
    usage_monthly: UsageBuckets,
    /// Fixed UTC offset for usage day/month bucketing in tests. `None` in
    /// production — each event buckets with the offset in force at ITS
    /// timestamp ([`crate::tui::format::local_offset_secs`]), so replayed
    /// history lands on its original local calendar day across DST changes.
    usage_offset_override: Option<i64>,
    /// Front = newest (the log renders newest-top).
    completed: VecDeque<Completed>,
    totals: HashMap<String, Totals>,
    /// Requests that finished before routing (no account) — kept out of the
    /// per-account map but included in the global totals.
    unrouted: Totals,
    /// Per (group, served_model) usage rows (req1-20). Keyed by the normalized
    /// served model within its backend group.
    models: HashMap<(String, String), ModelStats>,
    /// Per-client request attribution (issue #32), keyed by `metadata.user_id`
    /// (the `unknown` bucket holds requests with no id). In-memory only —
    /// runtime accounting, reset on restart, never persisted to disk. Bounded
    /// by [`MAX_CLIENTS`] distinct ids (the `unknown` bucket excluded). This is
    /// pure metering: counting requests/tokens per client, never gating.
    clients: HashMap<String, Totals>,
    /// Rolling hourly bucket ring for the windowed (24h/72h) per-account
    /// per-model heatmap (issue #23). In-memory only — durable persistence is a
    /// follow-up. Keyed by (group, normalized_model, account).
    windowed: WindowedBuckets,
    /// Per-second health buckets for the header health verdict (glance-triage
    /// MUST-FIX 3): the verdict window must NEVER be derived from the
    /// `completed` ring — [`LOG_CAPACITY`] truncation would undercount a storm
    /// exactly when accuracy matters most. Back = newest second; pruned to
    /// [`HEALTH_WINDOW`] on every push, at most [`HEALTH_BUCKET_CAP`] entries
    /// regardless of request rate.
    health: VecDeque<(u64, HealthCounts)>,
}

/// Status-class counts over the last [`HEALTH_WINDOW`], feeding the header
/// health verdict. Serialized 1:1 into the dashboard document so local and
/// attach render the identical verdict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HealthCounts {
    pub requests: u64,
    /// Status >= 400.
    pub errors: u64,
    pub s429: u64,
    pub s401: u64,
    pub s5xx: u64,
}

impl HealthCounts {
    /// Fold one finished request's status into the counts.
    fn add_status(&mut self, status: u16) {
        self.requests += 1;
        if status >= 400 {
            self.errors += 1;
        }
        match status {
            429 => self.s429 += 1,
            401 => self.s401 += 1,
            500..=599 => self.s5xx += 1,
            _ => {}
        }
    }

    fn merge(&mut self, other: &HealthCounts) {
        self.requests += other.requests;
        self.errors += other.errors;
        self.s429 += other.s429;
        self.s401 += other.s401;
        self.s5xx += other.s5xx;
    }
}

/// A finished per-client attribution row (issue #32): one client identity
/// (`metadata.user_id`, or `unknown`) and its lifetime request/token counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientUsage {
    pub client: String,
    pub requests: u64,
    pub ok: u64,
    pub errors: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

impl ActivityLog {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            ..Self::default()
        }
    }

    /// Replay a persisted activity log (req-persist A/C): read `path`
    /// line-by-line and fold every parseable [`PersistedRequest`] back through
    /// [`Self::apply`] at its original timestamp, rebuilding the cumulative
    /// model/account aggregates and seeding the activity ring. Same fold as the
    /// live path → the restored math is identical, no double-counting.
    ///
    /// Best-effort and total: a `None` path or missing file is a no-op; a line
    /// that is not valid JSON, or whose `v` is not the current
    /// [`PERSIST_VERSION`], is skipped (tolerating corruption and old formats);
    /// nothing here panics. The ring's capacity still bounds the in-memory
    /// `completed` view — replaying a huge log keeps the totals but only the
    /// newest `capacity` request lines stay visible (req C keeps the FILE
    /// complete; the ring is the display window).
    /// Production hydration goes through [`Self::load_persisted_prefix`] (the
    /// cut-bounded reader); this unbounded convenience wrapper remains for the
    /// replay round-trip tests.
    #[cfg(test)]
    pub(crate) fn load_persisted(&mut self, path: Option<&Path>) {
        let Some(path) = path else {
            return;
        };
        // Best-effort: a missing/unreadable file = nothing to resume from.
        let _ = self.load_persisted_prefix(path, u64::MAX);
    }

    /// [`Self::load_persisted`] bounded to the FIRST `up_to` bytes of the file.
    ///
    /// This is the double-count guard for background hydration: the daemon arms
    /// persistence and starts appending LIVE finished requests to the same file
    /// while history is still loading, so the loader must only replay what
    /// existed BEFORE arming — the byte length captured at arm time. Appends
    /// are whole lines, so the cut falls on a line boundary (a torn crash
    /// artifact straddling it parses as corrupt and is skipped, same as on the
    /// unbounded path).
    ///
    /// A missing file is `Ok` (first boot); any other IO error is returned so
    /// the caller can surface the degraded (empty-history) start.
    pub(crate) fn load_persisted_prefix(
        &mut self,
        path: &Path,
        up_to: u64,
    ) -> Result<(), std::io::Error> {
        use std::io::Read as _;
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        let mut contents = String::new();
        file.take(up_to).read_to_string(&mut contents)?;
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<PersistedRequest>(line) else {
                continue; // corrupt / not a PersistedRequest line
            };
            if record.v != PERSIST_VERSION {
                continue; // older/newer schema — skip rather than misread
            }
            let (event, ts) = record.into_event();
            self.apply(event, ts);
        }
        Ok(())
    }

    /// Merge a replayed HISTORY log behind this LIVE log (background hydration).
    ///
    /// The live log has been folding real traffic since boot; `history` is a
    /// fresh log that replayed the persisted records from before boot (strictly
    /// older than every live entry). Merge order is "history behind live":
    ///
    /// - `completed` ring: live rows stay in front, history extends behind
    ///   (both are newest-first and history is uniformly older), truncated to
    ///   this ring's capacity.
    /// - cumulative totals / model rows / client rows: summed — addition
    ///   commutes, so the result equals the old blocking replay-then-live fold.
    /// - windowed hourly buckets: merged by epoch hour, so history lands in its
    ///   ORIGINAL hours. (Folding old events through `apply` after live traffic
    ///   would instead dump them into the CURRENT hour — `roll_to` never
    ///   rewinds — inflating the heatmap; this by-hour merge is why hydration
    ///   must not simply replay into the live log.)
    /// - `in_flight` is untouched: history contains only finished requests, and
    ///   a live in-flight row whose id collides with a historical id must not
    ///   be swallowed by the replay's finish-matching.
    ///
    /// Returns how many historical requests were merged (for the operator note).
    pub(crate) fn merge_history_behind(&mut self, history: ActivityLog) -> u64 {
        let merged = history.totals_global().requests;
        // Ring: live in front, history behind, capacity kept.
        self.completed.extend(history.completed);
        self.completed.truncate(self.capacity);
        for (account, totals) in history.totals {
            self.totals.entry(account).or_default().add(&totals);
        }
        self.unrouted.add(&history.unrouted);
        for (key, stats) in history.models {
            self.models.entry(key).or_default().absorb(stats);
        }
        // Per-client rows respect the same MAX_CLIENTS bound as the live fold:
        // an unseen client past the cap folds into `unknown`.
        for (client, totals) in history.clients {
            let key = if client == UNKNOWN_CLIENT
                || self.clients.contains_key(&client)
                || self.clients.len() < MAX_CLIENTS
            {
                client
            } else {
                UNKNOWN_CLIENT.to_string()
            };
            self.clients.entry(key).or_default().add(&totals);
        }
        self.windowed.merge_behind(history.windowed);
        // Tokens-per-day buckets (UI-3 U14) merge BY DAY so history lands on
        // its original days — same reasoning as the hourly buckets above.
        for (day, cells) in history.daily {
            let dst = self.daily.entry(day).or_default();
            for (key, t) in cells {
                let cell = dst.entry(key).or_default();
                cell.input = cell.input.saturating_add(t.input);
                cell.output = cell.output.saturating_add(t.output);
                cell.cache_read = cell.cache_read.saturating_add(t.cache_read);
                cell.cache_creation = cell.cache_creation.saturating_add(t.cache_creation);
            }
        }
        // Usage-tab calendar buckets (usage-stats) merge BY BUCKET KEY so
        // history lands on its original hours/days/months — same reasoning
        // as the daily chart buckets above.
        for (src, dst) in [
            (history.usage_hourly, &mut self.usage_hourly),
            (history.usage_daily, &mut self.usage_daily),
            (history.usage_monthly, &mut self.usage_monthly),
        ] {
            for (bucket, cells) in src {
                let slot = dst.entry(bucket).or_default();
                for (key, cell) in cells {
                    slot.entry(key).or_default().merge(&cell);
                }
            }
        }
        // Session labels (UI-3 U2): first-seen wins, so a LIVE label beats the
        // replayed history for the same client; history fills the gaps under
        // the same MAX_CLIENTS bound as the live fold.
        for (uid, label) in history.session_labels {
            if !self.session_labels.contains_key(&uid) && self.session_labels.len() < MAX_CLIENTS {
                self.session_labels.insert(uid, label);
            }
        }
        merged
    }

    pub(crate) fn in_flight(&self) -> &[InFlight] {
        &self.in_flight
    }

    /// Sweep in-flight rows older than [`STALE_IN_FLIGHT`]: their
    /// `RequestFinished` event was almost certainly dropped on a full activity
    /// channel (the daemon reports the request as completed while the dashboard
    /// would otherwise show it pinned forever). Each swept row leaves a note so
    /// the cause is visible in the log rather than silently vanishing.
    ///
    /// Called on every dashboard read (`view`) and at the top of `apply` so a
    /// leaked row is bounded even with no further activity. Idempotent and
    /// cheap (a single retain over a ≤64-entry vec).
    pub(crate) fn prune_stale_in_flight(&mut self, now: SystemTime) {
        let mut swept: Vec<InFlight> = Vec::new();
        self.in_flight.retain(|entry| {
            let stale = now
                .duration_since(entry.started_at)
                .map(|age| age >= STALE_IN_FLIGHT)
                .unwrap_or(false);
            if stale {
                swept.push(entry.clone());
            }
            !stale
        });
        for entry in swept {
            self.push_note(
                format!(
                    "{} {} presumed finished (activity event dropped)",
                    entry.method, entry.path
                ),
                true,
                now,
            );
        }
    }

    /// Completed entries, newest first.
    pub(crate) fn completed(&self) -> impl Iterator<Item = &Completed> {
        self.completed.iter()
    }

    /// Derived session titles (TUI UI-3 U2): client id → first user-input
    /// excerpt. Cloned for the dashboard document.
    pub(crate) fn session_labels(&self) -> HashMap<String, String> {
        self.session_labels.clone()
    }

    /// Tokens-per-day rows (UI-3 U14), oldest day first. Flattened for the
    /// dashboard document.
    pub(crate) fn daily_usage(&self) -> Vec<crate::dashboard::DailyUsageDoc> {
        self.daily
            .iter()
            .flat_map(|(day, cells)| {
                cells
                    .iter()
                    .map(move |((group, model), t)| crate::dashboard::DailyUsageDoc {
                        day: *day,
                        group: group.clone(),
                        model: model.clone(),
                        tokens_in: t.input,
                        tokens_out: t.output,
                        cache_read: t.cache_read,
                        cache_creation: t.cache_creation,
                    })
            })
            .collect()
    }

    /// Fold one attributed finished request into the Usage-tab calendar
    /// buckets (usage-stats). `now` is the event fold time — the startup
    /// replay passes each record's PERSISTED timestamp, so history lands on
    /// its real hour/day/month. Day/month keys use the LOCAL civil calendar
    /// at the offset in force at that instant (tests pin a fixed offset via
    /// [`Self::set_usage_offset`]).
    fn record_usage(
        &mut self,
        group: &str,
        model: &str,
        tokens: Option<TokenCounts>,
        now: SystemTime,
    ) {
        let epoch_secs = match now.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs(),
            Err(_) => return,
        };
        let offset = self
            .usage_offset_override
            .unwrap_or_else(|| crate::tui::format::local_offset_secs(now));
        let key = (group.to_string(), model.to_string());

        let hour = epoch_secs / 3_600;
        self.usage_hourly
            .entry(hour)
            .or_default()
            .entry(key.clone())
            .or_default()
            .add(tokens);
        let hour_cutoff = hour.saturating_sub(USAGE_HOURLY_RETAIN_HOURS - 1);
        self.usage_hourly.retain(|h, _| *h >= hour_cutoff);

        // Local civil day: shift the clock by the offset, then count whole
        // days. Negative locals (pre-1970 west-of-UTC edge) can't occur for
        // real traffic; guard with euclid division anyway.
        let local_secs = (epoch_secs as i64).saturating_add(offset);
        let civil_day = local_secs.div_euclid(86_400);
        let day = u64::try_from(civil_day).unwrap_or(0);
        self.usage_daily
            .entry(day)
            .or_default()
            .entry(key.clone())
            .or_default()
            .add(tokens);
        let day_cutoff = day.saturating_sub(USAGE_DAILY_RETAIN_DAYS - 1);
        self.usage_daily.retain(|d, _| *d >= day_cutoff);

        let (year, month, _) = crate::tui::format::civil_from_days(civil_day);
        let month_key = u64::try_from(year.saturating_mul(12) + i64::from(month) - 1).unwrap_or(0);
        self.usage_monthly
            .entry(month_key)
            .or_default()
            .entry(key)
            .or_default()
            .add(tokens);
    }

    /// Pin a fixed UTC offset for usage day/month bucketing (tests only —
    /// production buckets with the per-event local offset).
    #[cfg(test)]
    pub(crate) fn set_usage_offset(&mut self, offset_secs: i64) {
        self.usage_offset_override = Some(offset_secs);
    }

    /// Usage-tab rows (usage-stats), flattened for the dashboard document:
    /// every granularity, newest bucket first, models within a bucket sorted
    /// by (group, model) so the document is deterministic (the renderer
    /// re-sorts by cost). Labels are rendered HERE — the daemon's civil
    /// calendar is the single source of truth for what "a day" means, so
    /// local and attach clients can never disagree.
    pub(crate) fn usage_stats(&self) -> Vec<crate::dashboard::UsageStatDoc> {
        fn sorted(
            cells: &HashMap<(String, String), UsageCell>,
        ) -> Vec<(&(String, String), &UsageCell)> {
            let mut v: Vec<_> = cells.iter().collect();
            v.sort_by(|a, b| a.0.cmp(b.0));
            v
        }
        let row = |gran: UsageGran,
                   bucket: u64,
                   label: String,
                   (group, model): &(String, String),
                   c: &UsageCell| {
            crate::dashboard::UsageStatDoc {
                gran: gran.tag().to_string(),
                bucket,
                label,
                group: group.clone(),
                model: model.clone(),
                requests: c.requests,
                tokens_in: c.input,
                tokens_out: c.output,
                cache_read: c.cache_read,
                cache_creation: c.cache_creation,
                cost_usd: 0.0,
            }
        };
        let mut docs = Vec::new();
        for (hour, cells) in self.usage_hourly.iter().rev() {
            // Hour buckets are UTC hours; label them in the daemon's local
            // wall clock at the bucket start.
            let start = UNIX_EPOCH + Duration::from_secs(hour * 3_600);
            let offset = self
                .usage_offset_override
                .unwrap_or_else(|| crate::tui::format::local_offset_secs(start));
            let local = (hour * 3_600) as i64 + offset;
            let (_, month, day) = crate::tui::format::civil_from_days(local.div_euclid(86_400));
            let hh = local.rem_euclid(86_400) / 3_600;
            let label = format!("{month:02}-{day:02} {hh:02}h");
            for (key, cell) in sorted(cells) {
                docs.push(row(UsageGran::Hour, *hour, label.clone(), key, cell));
            }
        }
        for (day, cells) in self.usage_daily.iter().rev() {
            let (y, m, d) = crate::tui::format::civil_from_days(*day as i64);
            let label = format!("{y}-{m:02}-{d:02}");
            for (key, cell) in sorted(cells) {
                docs.push(row(UsageGran::Day, *day, label.clone(), key, cell));
            }
        }
        for (month_key, cells) in self.usage_monthly.iter().rev() {
            let y = month_key / 12;
            let m = month_key % 12 + 1;
            let label = format!("{y}-{m:02}");
            for (key, cell) in sorted(cells) {
                docs.push(row(UsageGran::Month, *month_key, label.clone(), key, cell));
            }
        }
        docs
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

    /// Fold one attributed completed request into its `(group, model)` row.
    #[allow(clippy::too_many_arguments)]
    fn record_model(
        &mut self,
        group: &str,
        model: &str,
        account: &Option<String>,
        status: u16,
        tokens: Option<TokenCounts>,
        effort: &Option<String>,
        path: &str,
        now: SystemTime,
    ) {
        let entry = self
            .models
            .entry((group.to_string(), normalize_model(model)))
            .or_default();
        entry.requests += 1;
        if status < 400 {
            entry.ok += 1;
        } else {
            entry.errors += 1;
        }
        if let Some(t) = tokens {
            entry.tokens_in = entry.tokens_in.saturating_add(t.input);
            entry.tokens_out = entry.tokens_out.saturating_add(t.output);
            entry.cache_read = crate::proxy::sse::add_opt(entry.cache_read, t.cache_read);
            entry.cache_creation =
                crate::proxy::sse::add_opt(entry.cache_creation, t.cache_creation);
        }
        entry.last_used = Some(now);
        let effort_label = effort.clone().unwrap_or_else(|| "none".to_string());
        *entry.efforts.entry(effort_label).or_default() += 1;
        *entry.endpoints.entry(endpoint_class(path)).or_default() += 1;
        if let Some(name) = account {
            let at = entry.accounts.entry(name.clone()).or_default();
            at.requests += 1;
            if status < 400 {
                at.ok += 1;
            } else {
                at.errors += 1;
            }
            if let Some(t) = tokens {
                at.tokens_in = at.tokens_in.saturating_add(t.input);
                at.tokens_out = at.tokens_out.saturating_add(t.output);
            }
            // Fold the same request into the windowed bucket ring (issue #23).
            // Only account-attributed requests get a per-account cell; the key
            // carries group AND normalized model AND account so providers and
            // accounts never merge. `group`/`model` are normalized inside.
            self.windowed
                .record(group, model, name, status, tokens, now);
        }
    }

    /// Fold one finished request into its per-client bucket (issue #32).
    /// `user_id` is the `metadata.user_id` (or `None` → the `unknown` bucket).
    /// Bounded by [`MAX_CLIENTS`]: once that many distinct ids are tracked, a
    /// brand-new id is merged into `unknown` rather than allocating a new
    /// entry (already-tracked ids and `unknown` always accumulate). This is
    /// counting only — it never affects whether the request was served.
    fn record_client(&mut self, user_id: Option<&str>, status: u16, tokens: Option<TokenCounts>) {
        let key = match user_id {
            Some(id) if !id.is_empty() => {
                // Cap distinct named clients: an unseen id past the cap is
                // folded into `unknown` so the map cannot grow unbounded.
                if self.clients.contains_key(id) || self.clients.len() < MAX_CLIENTS {
                    id.to_string()
                } else {
                    UNKNOWN_CLIENT.to_string()
                }
            }
            _ => UNKNOWN_CLIENT.to_string(),
        };
        let bucket = self.clients.entry(key).or_default();
        bucket.requests += 1;
        if status < 400 {
            bucket.ok += 1;
        } else {
            bucket.errors += 1;
        }
        if let Some(t) = tokens {
            bucket.tokens_in = bucket.tokens_in.saturating_add(t.input);
            bucket.tokens_out = bucket.tokens_out.saturating_add(t.output);
        }
    }

    /// Per-client attribution lookup (issue #32), exercised by the tests.
    #[cfg(test)]
    pub(crate) fn client_totals(&self, client: &str) -> Totals {
        self.clients.get(client).copied().unwrap_or_default()
    }

    /// Snapshot of every per-client attribution row (issue #32), sorted by
    /// requests desc, then total tokens desc, then client name. The `unknown`
    /// bucket sorts by the same key as any other (it is just another client).
    pub(crate) fn client_usage(&self) -> Vec<ClientUsage> {
        let mut rows: Vec<ClientUsage> = self
            .clients
            .iter()
            .map(|(client, t)| ClientUsage {
                client: client.clone(),
                requests: t.requests,
                ok: t.ok,
                errors: t.errors,
                tokens_in: t.tokens_in,
                tokens_out: t.tokens_out,
            })
            .collect();
        rows.sort_by(|a, b| {
            b.requests
                .cmp(&a.requests)
                .then((b.tokens_in + b.tokens_out).cmp(&(a.tokens_in + a.tokens_out)))
                .then(a.client.cmp(&b.client))
        });
        rows
    }

    /// Snapshot of every model row, sorted by total tokens (fresh input +
    /// output + cache read + cache write) desc, then requests, then key
    /// (req14). Cache tokens count so the ranking matches the strip's `tok`
    /// column (`ui::model_total`), which includes them. The document builder
    /// overlays in-flight counts.
    pub(crate) fn model_usage(&self) -> Vec<ModelUsage> {
        let mut rows: Vec<ModelUsage> = self
            .models
            .iter()
            .map(|((group, model), stats)| {
                let mut accounts: Vec<ModelAccount> = stats
                    .accounts
                    .iter()
                    .map(|(name, t)| ModelAccount {
                        name: name.clone(),
                        requests: t.requests,
                        ok: t.ok,
                        errors: t.errors,
                        tokens_in: t.tokens_in,
                        tokens_out: t.tokens_out,
                    })
                    .collect();
                accounts.sort_by(|a, b| b.requests.cmp(&a.requests).then(a.name.cmp(&b.name)));
                ModelUsage {
                    group: group.clone(),
                    model: model.clone(),
                    requests: stats.requests,
                    ok: stats.ok,
                    errors: stats.errors,
                    tokens_in: stats.tokens_in,
                    tokens_out: stats.tokens_out,
                    cache_read: stats.cache_read,
                    cache_creation: stats.cache_creation,
                    last_used: stats.last_used.unwrap_or(SystemTime::UNIX_EPOCH),
                    accounts,
                    efforts: sorted_counts(&stats.efforts),
                    endpoints: sorted_counts(&stats.endpoints),
                }
            })
            .collect();
        let total = |r: &ModelUsage| {
            r.tokens_in
                .saturating_add(r.tokens_out)
                .saturating_add(r.cache_read.unwrap_or(0))
                .saturating_add(r.cache_creation.unwrap_or(0))
        };
        rows.sort_by(|a, b| {
            total(b)
                .cmp(&total(a))
                .then(b.requests.cmp(&a.requests))
                .then(a.group.cmp(&b.group))
                .then(a.model.cmp(&b.model))
        });
        rows
    }

    /// Aggregate the windowed bucket ring over `window` ending at `now`: one
    /// row per `(group, normalized_model, account)` with any activity in the
    /// window, sorted by total tokens desc (issue #23). Drives the heatmap.
    /// Best-effort — the underlying events are a lossy sample (dropped on a full
    /// activity channel), so these numbers may undercount.
    pub(crate) fn windowed_rows(&self, window: StatsWindow, now: SystemTime) -> Vec<WindowedRow> {
        self.windowed.aggregate(window, now)
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
        // Backstop against a dropped `RequestFinished`: any row older than the
        // stale threshold is presumed finished before we fold the next event.
        self.prune_stale_in_flight(now);
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
                    group: None,
                    model: None,
                    effort: None,
                    fast: false,
                    started_at: now,
                });
            }
            ActivityEvent::RequestRouted {
                id,
                account,
                group,
                model,
                effort,
                fast,
            } => {
                if let Some(entry) = self.in_flight.iter_mut().find(|r| r.id == id) {
                    entry.account = Some(account);
                    entry.group = group;
                    entry.model = model;
                    entry.effort = effort;
                    entry.fast = fast;
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
                fast,
                user_id,
                kind,
                excerpt,
            } => {
                let routed = self
                    .in_flight
                    .iter()
                    .position(|r| r.id == id)
                    .map(|i| self.in_flight.remove(i))
                    .and_then(|r| r.account);
                let account = account.or(routed);
                // Per-client attribution (issue #32): every finished request is
                // counted against its `metadata.user_id` client bucket (the
                // `unknown` bucket when absent), independent of routing — so
                // pre-routing failures are attributed too, never dropped.
                self.record_client(user_id.as_deref(), status, tokens);
                // Session label (TUI UI-3 U2): the FIRST plain user-input
                // excerpt seen for a client id becomes that session's derived
                // title (nothing on the wire carries a real one). Bounded by
                // MAX_CLIENTS via the same insert-guard as client buckets.
                if let (Some(uid), Some("user"), Some(text)) =
                    (user_id.as_deref(), kind.as_deref(), excerpt.as_deref())
                {
                    if !self.session_labels.contains_key(uid)
                        && self.session_labels.len() < MAX_CLIENTS
                    {
                        self.session_labels
                            .insert(uid.to_string(), text.chars().take(48).collect());
                    }
                }
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
                // Model-usage aggregation (req1-20): only when the request was
                // attributed to a (group, model). Pre-routing failures keep
                // group/model None and stay in the global/unrouted accounting
                // above — no bogus model row. A failed-but-attributed request
                // still increments the row's error count even with no tokens.
                if let (Some(group), Some(model)) = (&group, &model) {
                    self.record_model(group, model, &account, status, tokens, &effort, &path, now);
                    // Tokens-per-day chart fold (UI-3 U14). `now` is the event
                    // fold time — the startup replay passes each record's
                    // PERSISTED timestamp, so history lands on its real day.
                    if let Some(t) = tokens {
                        let day = now
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs() / 86_400)
                            .unwrap_or(0);
                        let cell = self
                            .daily
                            .entry(day)
                            .or_default()
                            .entry((group.clone(), model.clone()))
                            .or_default();
                        cell.input = cell.input.saturating_add(t.input);
                        cell.output = cell.output.saturating_add(t.output);
                        cell.cache_read = cell.cache_read.saturating_add(t.cache_read.unwrap_or(0));
                        cell.cache_creation = cell
                            .cache_creation
                            .saturating_add(t.cache_creation.unwrap_or(0));
                        // Keep exactly DAILY_RETAIN_DAYS buckets including
                        // today (cutoff inclusive — the -1 avoids a 91-day
                        // window, review R1 nice-to-have 2).
                        let cutoff = day.saturating_sub(DAILY_RETAIN_DAYS - 1);
                        self.daily.retain(|d, _| *d >= cutoff);
                    }
                    // Usage-tab calendar fold (usage-stats): unlike the chart
                    // fold above, EVERY attributed finished request lands in
                    // its hour/day/month bucket (tokens or not), so the
                    // requests column matches the per-model row semantics.
                    self.record_usage(group, model, tokens, now);
                }
                self.push_health(now, status);
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
                        fast,
                        user_id,
                        kind,
                        excerpt,
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

    /// Record one finished request into its per-second health bucket and
    /// prune buckets that have aged out of [`HEALTH_WINDOW`].
    fn push_health(&mut self, at: SystemTime, status: u16) {
        let sec = at
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        match self.health.back_mut() {
            // Same (or out-of-order, sub-second reordering) second: fold into
            // the newest bucket — delivery is chronological, so this only
            // ever merges same/adjacent-second arrivals.
            Some((bucket_sec, counts)) if *bucket_sec >= sec => counts.add_status(status),
            _ => {
                let mut counts = HealthCounts::default();
                counts.add_status(status);
                self.health.push_back((sec, counts));
            }
        }
        let newest = self.health.back().map(|&(sec, _)| sec).unwrap_or(0);
        let horizon = newest.saturating_sub(HEALTH_WINDOW.as_secs());
        while let Some(&(front, _)) = self.health.front() {
            if front < horizon || self.health.len() > HEALTH_BUCKET_CAP {
                self.health.pop_front();
            } else {
                break;
            }
        }
    }

    /// Status-class counts over the last [`HEALTH_WINDOW`] ending at `now`.
    /// Reads filter by time (pruning happens on push), so a quiet log still
    /// ages out: a bucket counts only while `now - HEALTH_WINDOW <= at`.
    pub(crate) fn health_counts(&self, now: SystemTime) -> HealthCounts {
        let now_sec = now
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let horizon = now_sec.saturating_sub(HEALTH_WINDOW.as_secs());
        let mut total = HealthCounts::default();
        for &(sec, counts) in self.health.iter().rev() {
            if sec < horizon {
                break;
            }
            total.merge(&counts);
        }
        total
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn hydration_merges_daily_buckets_and_session_labels_behind_live() {
        use std::time::{Duration, UNIX_EPOCH};
        let day = |d: u64| UNIX_EPOCH + Duration::from_secs(d * 86_400 + 3600);
        let finished =
            |_ts: u64, tok_in: u64, uid: &str, excerpt: &str| ActivityEvent::RequestFinished {
                id: 1,
                method: "POST".into(),
                path: "/v1/messages".into(),
                account: Some("a".into()),
                status: 200,
                duration: Duration::from_millis(100),
                tokens: Some(TokenCounts {
                    input: tok_in,
                    output: 10,
                    cache_read: None,
                    cache_creation: None,
                }),
                group: Some("claude".into()),
                model: Some("claude-fable-5".into()),
                effort: None,
                fast: false,
                user_id: Some(uid.into()),
                kind: Some("user".into()),
                excerpt: Some(excerpt.into()),
            };
        let _ = day;
        let mut live = ActivityLog::new(16);
        live.apply(finished(0, 100, "s1", "live first input"), day(20_000));
        let mut history = ActivityLog::new(16);
        history.apply(finished(0, 40, "s1", "old input"), day(19_999));
        history.apply(finished(0, 70, "s2", "history session"), day(19_998));
        live.merge_history_behind(history);
        let daily = live.daily_usage();
        // Three distinct days, each with its own bucket — history landed on
        // its ORIGINAL days.
        assert_eq!(daily.len(), 3);
        assert!(daily.iter().any(|r| r.day == 19_998 && r.tokens_in == 70));
        assert!(daily.iter().any(|r| r.day == 20_000 && r.tokens_in == 100));
        // Live label wins for s1; history fills s2.
        let labels = live.session_labels();
        assert_eq!(
            labels.get("s1").map(String::as_str),
            Some("live first input")
        );
        assert_eq!(
            labels.get("s2").map(String::as_str),
            Some("history session")
        );
    }
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
            tokens: tokens.map(|(input, output)| TokenCounts {
                input,
                output,
                ..Default::default()
            }),
            group: None,
            model: None,
            effort: None,
            fast: false,
            user_id: None,
            kind: None,
            excerpt: None,
        }
    }

    /// A finished request attributed to a `(group, model)`, with optional
    /// effort and cache counters, for the model-aggregation tests.
    #[allow(clippy::too_many_arguments)]
    fn finished_model(
        id: u64,
        account: Option<&str>,
        group: &str,
        model: &str,
        effort: Option<&str>,
        status: u16,
        tokens: Option<TokenCounts>,
        path: &str,
    ) -> ActivityEvent {
        ActivityEvent::RequestFinished {
            id,
            method: "POST".into(),
            path: path.into(),
            account: account.map(str::to_string),
            status,
            duration: Duration::from_millis(1_400),
            tokens,
            group: Some(group.into()),
            model: Some(model.into()),
            effort: effort.map(str::to_string),
            fast: false,
            user_id: None,
            kind: None,
            excerpt: None,
        }
    }

    /// A finished request carrying a `metadata.user_id` (or `None`) for the
    /// per-client attribution tests (issue #32). Minimal otherwise.
    fn finished_client(
        id: u64,
        user_id: Option<&str>,
        tokens: Option<(u64, u64)>,
        status: u16,
    ) -> ActivityEvent {
        ActivityEvent::RequestFinished {
            id,
            method: "POST".into(),
            path: "/v1/messages".into(),
            account: None,
            status,
            duration: Duration::from_millis(1_400),
            tokens: tokens.map(|(input, output)| TokenCounts {
                input,
                output,
                ..Default::default()
            }),
            group: None,
            model: None,
            effort: None,
            fast: false,
            user_id: user_id.map(str::to_string),
            kind: None,
            excerpt: None,
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

        assert_eq!(log.in_flight()[0].effort, None, "unknown before routing");
        assert!(!log.in_flight()[0].fast, "fast off before routing");

        log.apply(
            ActivityEvent::RequestRouted {
                id: 7,
                account: "a@x.com".into(),
                group: Some("claude".into()),
                model: Some("claude-sonnet-4-5".into()),
                effort: Some("low".into()),
                fast: true,
            },
            at(1),
        );
        assert_eq!(log.in_flight()[0].account.as_deref(), Some("a@x.com"));
        assert_eq!(log.in_flight()[0].group.as_deref(), Some("claude"));
        assert_eq!(
            log.in_flight()[0].model.as_deref(),
            Some("claude-sonnet-4-5")
        );
        // The routed event's per-request effort/fast land on the in-flight row
        // so the running badge matches the eventual completed badge.
        assert_eq!(log.in_flight()[0].effort.as_deref(), Some("low"));
        assert!(log.in_flight()[0].fast);

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
                        ..Default::default()
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

    #[test]
    fn prune_stale_in_flight_sweeps_rows_past_threshold_with_a_note() {
        let mut log = ActivityLog::new(200);
        log.apply(started(1), at(0));
        assert_eq!(log.in_flight().len(), 1, "row is in-flight");

        // Still fresh just before the threshold: nothing swept.
        log.prune_stale_in_flight(at(STALE_IN_FLIGHT.as_secs() - 1));
        assert_eq!(log.in_flight().len(), 1, "not yet stale");

        // Advance past the stale threshold (real requests finish in <90s, so a
        // row this old means its RequestFinished was dropped).
        log.prune_stale_in_flight(at(STALE_IN_FLIGHT.as_secs() + 1));
        assert!(
            log.in_flight().is_empty(),
            "stale row swept, no zombie left"
        );
        let entry = log.completed().next().expect("sweep note").clone();
        match &entry.body {
            CompletedBody::Note { text, error } => {
                assert!(error, "sweep note is an error note");
                assert!(
                    text.contains("presumed finished"),
                    "note names the cause, got {text:?}"
                );
            }
            other => panic!("expected note, got {other:?}"),
        }
    }

    #[test]
    fn apply_sweeps_stale_in_flight_before_folding_next_event() {
        let mut log = ActivityLog::new(200);
        log.apply(started(1), at(0));
        // A later, unrelated event arriving past the threshold sweeps the
        // leaked row even though no RequestFinished for id 1 ever came.
        log.apply(started(2), at(STALE_IN_FLIGHT.as_secs() + 5));
        assert!(
            !log.in_flight().iter().any(|r| r.id == 1),
            "leaked row 1 swept on the next apply"
        );
        assert!(
            log.in_flight().iter().any(|r| r.id == 2),
            "fresh row 2 still in-flight"
        );
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

    // ---- health window (glance-triage) ----

    #[test]
    fn health_counts_survive_ring_truncation() {
        // A storm 100 events past LOG_CAPACITY: the completed ring truncates,
        // the health window must NOT (that undercount was the reason the
        // verdict gets its own deque — MUST-FIX 3).
        let mut log = ActivityLog::new(LOG_CAPACITY);
        let total = (LOG_CAPACITY + 100) as u64;
        for i in 0..total {
            log.apply(finished_status(i, Some("a"), None, 429), at(100 + i));
        }
        let now = at(100 + total);
        assert_eq!(log.completed().count(), LOG_CAPACITY);
        let counts = log.health_counts(now);
        assert_eq!(counts.requests, total);
        assert_eq!(counts.s429, total);
        assert_eq!(counts.errors, total);
    }

    #[test]
    fn health_counts_age_out_of_the_window() {
        let mut log = ActivityLog::new(10);
        log.apply(finished_status(1, Some("a"), None, 500), at(1_000 - 400)); // aged out
        log.apply(finished_status(2, Some("a"), None, 429), at(1_000 - 100));
        log.apply(finished_status(3, Some("a"), None, 200), at(1_000 - 10));
        let counts = log.health_counts(at(1_000));
        assert_eq!(counts.requests, 2);
        assert_eq!(counts.s429, 1);
        assert_eq!(counts.s5xx, 0, "outside the 5m window");
        assert_eq!(counts.errors, 1);
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

    // ---- model usage aggregation ----

    fn tokens(input: u64, output: u64, cache_read: Option<u64>) -> Option<TokenCounts> {
        Some(TokenCounts {
            input,
            output,
            cache_read,
            cache_creation: None,
        })
    }

    #[test]
    fn endpoint_class_buckets_count_tokens_messages_and_other() {
        assert_eq!(endpoint_class("/v1/messages"), "messages");
        assert_eq!(endpoint_class("/v1/messages?beta=true"), "messages");
        assert_eq!(endpoint_class("/v1/messages/count_tokens"), "count_tokens");
        assert_eq!(endpoint_class("/v1/models"), "models");
    }

    #[test]
    fn normalize_model_strips_context_suffix() {
        assert_eq!(
            normalize_model("claude-sonnet-4-5[1m]"),
            "claude-sonnet-4-5"
        );
        assert_eq!(normalize_model("gpt-5.5"), "gpt-5.5");
    }

    #[test]
    fn model_rows_key_by_group_and_served_model() {
        let mut log = ActivityLog::new(50);
        // Same label, different providers → two rows, never merged (req1/2).
        log.apply(
            finished_model(
                1,
                Some("a"),
                "claude",
                "shared",
                None,
                200,
                tokens(10, 5, None),
                "/v1/messages",
            ),
            at(1),
        );
        log.apply(
            finished_model(
                2,
                Some("c"),
                "codex",
                "shared",
                None,
                200,
                tokens(20, 7, None),
                "/v1/messages",
            ),
            at(2),
        );
        let rows = log.model_usage();
        assert_eq!(rows.len(), 2);
        // Sorted by total tokens desc → codex (27) before claude (15).
        assert_eq!(
            (rows[0].group.as_str(), rows[0].model.as_str()),
            ("codex", "shared")
        );
        assert_eq!(
            (rows[1].group.as_str(), rows[1].model.as_str()),
            ("claude", "shared")
        );
    }

    #[test]
    fn model_rows_sort_counts_cache_tokens() {
        let mut log = ActivityLog::new(50);
        // codex: 100 fresh in + 50 out = 150, no cache.
        log.apply(
            finished_model(
                1,
                Some("c"),
                "codex",
                "gpt-5.5",
                None,
                200,
                tokens(100, 50, None),
                "/v1/messages",
            ),
            at(1),
        );
        // claude: only 10 fresh in + 5 out, but 1_000 cache-read tokens →
        // 1_015 total. Cache tokens count toward the "by tokens" ranking
        // (they are what the `tok` column shows and the `$` column prices),
        // so the cache-heavy row ranks FIRST despite the smaller fresh sum.
        log.apply(
            finished_model(
                2,
                Some("a"),
                "claude",
                "claude-opus-4-8",
                None,
                200,
                tokens(10, 5, Some(1_000)),
                "/v1/messages",
            ),
            at(2),
        );
        let rows = log.model_usage();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].group.as_str(), "claude");
        assert_eq!(rows[1].group.as_str(), "codex");
    }

    #[test]
    fn model_row_accumulates_split_cache_effort_endpoint_and_accounts() {
        let mut log = ActivityLog::new(50);
        log.apply(
            finished_model(
                1,
                Some("a"),
                "claude",
                "claude-sonnet-4-5[1m]",
                Some("16k"),
                200,
                tokens(100, 40, Some(900)),
                "/v1/messages",
            ),
            at(10),
        );
        log.apply(
            finished_model(
                2,
                Some("b"),
                "claude",
                "claude-sonnet-4-5",
                None,
                200,
                tokens(50, 20, None),
                "/v1/messages/count_tokens",
            ),
            at(20),
        );
        // A failed request with a known model: error count, no tokens (req-test).
        log.apply(
            finished_model(
                3,
                Some("a"),
                "claude",
                "claude-sonnet-4-5",
                Some("16k"),
                529,
                None,
                "/v1/messages",
            ),
            at(30),
        );
        let rows = log.model_usage();
        // Suffix normalization merges into one row (req17).
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.requests, 3);
        assert_eq!(row.ok, 2);
        assert_eq!(row.errors, 1);
        assert_eq!(row.tokens_in, 150);
        assert_eq!(row.tokens_out, 60);
        // cache_read present from req1 only; cache_creation never reported.
        assert_eq!(row.cache_read, Some(900));
        assert_eq!(row.cache_creation, None);
        assert_eq!(row.last_used, at(30));
        // Effort distribution: 16k×2, none×1.
        let effort: HashMap<&str, u64> = row
            .efforts
            .iter()
            .map(|c| (c.label.as_str(), c.requests))
            .collect();
        assert_eq!(effort.get("16k"), Some(&2));
        assert_eq!(effort.get("none"), Some(&1));
        // Endpoint split: messages×2, count_tokens×1.
        let endpoint: HashMap<&str, u64> = row
            .endpoints
            .iter()
            .map(|c| (c.label.as_str(), c.requests))
            .collect();
        assert_eq!(endpoint.get("messages"), Some(&2));
        assert_eq!(endpoint.get("count_tokens"), Some(&1));
        // Per-account: a served 2 (one failed), b served 1.
        let a = row
            .accounts
            .iter()
            .find(|x| x.name == "a")
            .expect("account a");
        assert_eq!((a.requests, a.ok, a.errors, a.tokens_in), (2, 1, 1, 100));
        let b = row
            .accounts
            .iter()
            .find(|x| x.name == "b")
            .expect("account b");
        assert_eq!((b.requests, b.tokens_in), (1, 50));
    }

    #[test]
    fn pre_routing_failure_does_not_create_a_model_row() {
        let mut log = ActivityLog::new(50);
        // No group/model (body-read failure): global accounting only, no row.
        log.apply(finished_status(1, None, None, 400), at(1));
        assert!(log.model_usage().is_empty());
        assert_eq!(log.totals_global().requests, 1);
    }

    // ---- per-client attribution (issue #32) ----

    #[test]
    fn client_attribution_counts_requests_and_tokens_per_user_id() {
        let mut log = ActivityLog::new(50);
        // alice: 2 requests (one a 502 error), 300 in / 110 out total.
        log.apply(
            finished_client(1, Some("alice"), Some((100, 40)), 200),
            at(1),
        );
        log.apply(
            finished_client(2, Some("alice"), Some((200, 70)), 502),
            at(2),
        );
        // bob: 1 ok request.
        log.apply(finished_client(3, Some("bob"), Some((10, 5)), 200), at(3));
        // Two requests with NO user_id land in the explicit `unknown` bucket
        // (one carries no tokens), never dropped.
        log.apply(finished_client(4, None, Some((7, 3)), 200), at(4));
        log.apply(finished_client(5, None, None, 200), at(5));

        assert_eq!(
            log.client_totals("alice"),
            Totals {
                requests: 2,
                ok: 1,
                errors: 1,
                tokens_in: 300,
                tokens_out: 110,
            }
        );
        assert_eq!(
            log.client_totals("bob"),
            Totals {
                requests: 1,
                ok: 1,
                errors: 0,
                tokens_in: 10,
                tokens_out: 5,
            }
        );
        // No-user_id requests attributed to `unknown`, not dropped.
        assert_eq!(
            log.client_totals(UNKNOWN_CLIENT),
            Totals {
                requests: 2,
                ok: 2,
                errors: 0,
                tokens_in: 7,
                tokens_out: 3,
            }
        );
        // An empty-string user_id is treated as no id → `unknown`.
        log.apply(finished_client(6, Some(""), Some((1, 1)), 200), at(6));
        assert_eq!(log.client_totals(UNKNOWN_CLIENT).requests, 3);

        // client_usage() snapshot: three rows, sorted by requests desc — the
        // total across rows equals the number of finished requests (none lost).
        let rows = log.client_usage();
        assert_eq!(rows.len(), 3, "alice, bob, unknown");
        let total_requests: u64 = rows.iter().map(|r| r.requests).sum();
        assert_eq!(total_requests, 6, "every finished request attributed");
        // alice (2) and unknown (3) outrank bob (1); unknown leads on requests.
        assert_eq!(rows[0].client, UNKNOWN_CLIENT);
        assert_eq!(rows[0].requests, 3);
    }

    #[test]
    fn client_attribution_is_bounded_overflow_folds_into_unknown() {
        let mut log = ActivityLog::new(50);
        // Fill the named-client cap exactly with distinct ids.
        for i in 0..MAX_CLIENTS {
            log.apply(
                finished_client(i as u64, Some(&format!("c{i}")), None, 200),
                at(i as u64),
            );
        }
        assert_eq!(
            log.client_usage().len(),
            MAX_CLIENTS,
            "every distinct id under the cap gets its own bucket"
        );
        // A brand-new id past the cap does NOT allocate a new entry; it is
        // folded into `unknown` so the map cannot grow unbounded.
        log.apply(
            finished_client(9_000, Some("overflow"), None, 200),
            at(9_000),
        );
        assert_eq!(
            log.client_totals("overflow"),
            Totals::default(),
            "over-cap id is not tracked on its own"
        );
        assert_eq!(
            log.client_totals(UNKNOWN_CLIENT).requests,
            1,
            "over-cap id folded into unknown"
        );
        // An ALREADY-tracked id keeps accumulating even past the cap.
        log.apply(finished_client(9_001, Some("c0"), None, 200), at(9_001));
        assert_eq!(log.client_totals("c0").requests, 2);
    }

    // ---- windowed bucket ring (issue #23) ----

    /// One bucketed (group, model, account) cell within a window's aggregate.
    fn cell<'a>(
        rows: &'a [WindowedRow],
        group: &str,
        model: &str,
        account: &str,
    ) -> Option<&'a WindowCounts> {
        rows.iter()
            .find(|r| r.group == group && r.model == model && r.account == account)
            .map(|r| &r.counts)
    }

    /// `now` `hours` after the epoch, for window arithmetic at hour resolution.
    fn at_hours(hours: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(hours * 3600)
    }

    #[test]
    fn windowed_aggregates_per_group_model_account_are_correct() {
        let mut log = ActivityLog::new(LOG_CAPACITY);
        // Three requests for claude/sonnet on account "a" inside the last hour.
        for id in 0..3u64 {
            log.apply(
                finished_model(
                    id,
                    Some("a"),
                    "claude",
                    "claude-sonnet-4-5[1m]", // suffix is normalized away
                    None,
                    200,
                    tokens(100, 40, Some(10)),
                    "/v1/messages",
                ),
                at_hours(100),
            );
        }
        // One failed request for the SAME model but account "b".
        log.apply(
            finished_model(
                10,
                Some("b"),
                "claude",
                "claude-sonnet-4-5",
                None,
                500,
                None,
                "/v1/messages",
            ),
            at_hours(100),
        );
        let now = at_hours(100);
        let rows = log.windowed_rows(StatsWindow::Day, now);

        let a = cell(&rows, "claude", "claude-sonnet-4-5", "a").expect("a cell");
        assert_eq!(a.requests, 3);
        assert_eq!(a.ok, 3);
        assert_eq!(a.errors, 0);
        assert_eq!(a.tokens_in, 300);
        assert_eq!(a.tokens_out, 120);
        assert_eq!(a.cache_read, 30);
        // tokens() = in + out + cache_read + cache_creation.
        assert_eq!(a.tokens(), 450);

        let b = cell(&rows, "claude", "claude-sonnet-4-5", "b").expect("b cell");
        assert_eq!((b.requests, b.ok, b.errors), (1, 0, 1));
        assert_eq!(b.tokens(), 0, "failed request carried no tokens");

        // Two distinct account cells for one (group, model).
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn windowed_same_model_under_two_groups_stays_two_rows() {
        let mut log = ActivityLog::new(LOG_CAPACITY);
        log.apply(
            finished_model(
                1,
                Some("a"),
                "claude",
                "shared",
                None,
                200,
                tokens(10, 5, None),
                "/v1/messages",
            ),
            at_hours(50),
        );
        log.apply(
            finished_model(
                2,
                Some("a"),
                "codex",
                "shared",
                None,
                200,
                tokens(20, 7, None),
                "/v1/messages",
            ),
            at_hours(50),
        );
        let rows = log.windowed_rows(StatsWindow::Day, at_hours(50));
        // Same model name, same account, different group → never merged.
        assert!(cell(&rows, "claude", "shared", "a").is_some());
        assert!(cell(&rows, "codex", "shared", "a").is_some());
        assert_eq!(rows.len(), 2, "dropping group would have merged to 1");
    }

    #[test]
    fn windowed_24h_and_72h_select_the_right_buckets() {
        let mut log = ActivityLog::new(LOG_CAPACITY);
        // t = 200h: an old request, ~50h before "now".
        log.apply(
            finished_model(
                1,
                Some("a"),
                "claude",
                "m",
                None,
                200,
                tokens(7, 0, None),
                "/v1/messages",
            ),
            at_hours(200),
        );
        // t = 240h: a request 10h before "now".
        log.apply(
            finished_model(
                2,
                Some("a"),
                "claude",
                "m",
                None,
                200,
                tokens(11, 0, None),
                "/v1/messages",
            ),
            at_hours(240),
        );
        let now = at_hours(250);

        // 24h window: only the t=240h request (10h ago) is inside.
        let day = log.windowed_rows(StatsWindow::Day, now);
        assert_eq!(cell(&day, "claude", "m", "a").expect("day").requests, 1);
        assert_eq!(cell(&day, "claude", "m", "a").expect("day").tokens_in, 11);

        // 72h window: both the 10h-ago and 50h-ago requests are inside.
        let three = log.windowed_rows(StatsWindow::ThreeDay, now);
        assert_eq!(cell(&three, "claude", "m", "a").expect("3d").requests, 2);
        assert_eq!(cell(&three, "claude", "m", "a").expect("3d").tokens_in, 18);
    }

    #[test]
    fn windowed_roll_forward_expires_old_buckets_and_prunes_empty_keys() {
        let mut log = ActivityLog::new(LOG_CAPACITY);
        // A stray/typo model key recorded long ago.
        log.apply(
            finished_model(
                1,
                Some("a"),
                "claude",
                "typo-model",
                None,
                200,
                tokens(5, 0, None),
                "/v1/messages",
            ),
            at_hours(10),
        );
        // Far in the future (well past the 73-bucket retention): record again so
        // roll-forward advances the ring past the stray key's bucket.
        log.apply(
            finished_model(
                2,
                Some("a"),
                "claude",
                "live-model",
                None,
                200,
                tokens(9, 0, None),
                "/v1/messages",
            ),
            at_hours(10 + BUCKET_COUNT as u64 + 5),
        );
        let now = at_hours(10 + BUCKET_COUNT as u64 + 5);

        // The stray key's bucket was pruned entirely (not zeroed) — it is gone
        // from every window, so a typo key cannot grow the ring unbounded.
        let three = log.windowed_rows(StatsWindow::ThreeDay, now);
        assert!(
            cell(&three, "claude", "typo-model", "a").is_none(),
            "expired bucket must be pruned, not retained as a zero key"
        );
        assert!(
            cell(&three, "claude", "live-model", "a").is_some(),
            "the recent key survives"
        );
        // Internally: no empty key map lingers (pruned wholesale).
        assert!(
            log.windowed.buckets.iter().all(|b| !b.counts.is_empty()),
            "no empty bucket retained after roll-forward"
        );
    }

    #[test]
    fn windowed_is_defensive_against_backwards_clock_skew() {
        let mut log = ActivityLog::new(LOG_CAPACITY);
        log.apply(
            finished_model(
                1,
                Some("a"),
                "claude",
                "m",
                None,
                200,
                tokens(10, 0, None),
                "/v1/messages",
            ),
            at_hours(100),
        );
        // A later event with an EARLIER timestamp (NTP step back) must not panic
        // and must still be counted somewhere in the ring.
        log.apply(
            finished_model(
                2,
                Some("a"),
                "claude",
                "m",
                None,
                200,
                tokens(3, 0, None),
                "/v1/messages",
            ),
            at_hours(99),
        );
        // A pre-epoch timestamp clamps to hour 0 rather than panicking.
        let rows = log.windowed_rows(StatsWindow::ThreeDay, at_hours(100));
        assert!(
            cell(&rows, "claude", "m", "a").is_some(),
            "skewed events still fold without panic"
        );
    }

    // ---- persistence (req-persist A/C) ----

    use std::path::{Path, PathBuf};

    /// Self-cleaning unique temp dir (no tempfile dev-dependency), mirroring
    /// the pattern in `config::tests` / `server::tests`.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "llmux-activity-test-{}-{}",
                std::process::id(),
                ulid::Ulid::new()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }
        fn file(&self) -> PathBuf {
            self.0.join("activity.jsonl")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A finished, fully-attributed request for the persistence round-trip:
    /// exercises account totals AND a `(group, model)` model row + cache split.
    #[allow(clippy::too_many_arguments)]
    fn finished_full(
        id: u64,
        account: &str,
        group: &str,
        model: &str,
        effort: Option<&str>,
        status: u16,
        input: u64,
        output: u64,
        cache_read: Option<u64>,
        path: &str,
    ) -> ActivityEvent {
        ActivityEvent::RequestFinished {
            id,
            method: "POST".into(),
            path: path.into(),
            account: Some(account.to_string()),
            status,
            duration: Duration::from_millis(1_234),
            tokens: Some(TokenCounts {
                input,
                output,
                cache_read,
                cache_creation: None,
            }),
            group: Some(group.into()),
            model: Some(model.into()),
            effort: effort.map(str::to_string),
            fast: false,
            // A per-client id so the persistence round-trip also exercises the
            // issue #32 client attribution (one client id per account here).
            user_id: Some(format!("client-{account}")),
            kind: None,
            excerpt: None,
        }
    }

    /// Apply the same events to a fresh log without persisting — the oracle the
    /// restored log must match exactly.
    fn live_log(events: &[(ActivityEvent, SystemTime)]) -> ActivityLog {
        let mut log = ActivityLog::new(LOG_CAPACITY);
        for (event, ts) in events {
            log.apply(event.clone(), *ts);
        }
        log
    }

    /// Persist each event to `path`, then load a FRESH log from it.
    fn persisted_then_loaded(path: &Path, events: &[(ActivityEvent, SystemTime)]) -> ActivityLog {
        for (event, ts) in events {
            persist_request(Some(path), event, *ts);
        }
        let mut log = ActivityLog::new(LOG_CAPACITY);
        log.load_persisted(Some(path));
        log
    }

    /// Compare two logs on every persisted-aggregate surface: model_usage,
    /// global totals, and per-account totals. (model_usage carries last_used,
    /// cache split, accounts, efforts, endpoints — so equality here is strong.)
    fn assert_same_aggregates(a: &ActivityLog, b: &ActivityLog, accounts: &[&str]) {
        assert_eq!(
            a.model_usage(),
            b.model_usage(),
            "model_usage must match after restore"
        );
        assert_eq!(
            a.totals_global(),
            b.totals_global(),
            "global totals must match after restore"
        );
        assert_eq!(
            a.client_usage(),
            b.client_usage(),
            "per-client attribution must match after restore (issue #32)"
        );
        for acct in accounts {
            assert_eq!(
                a.totals_for(acct),
                b.totals_for(acct),
                "per-account totals for {acct} must match after restore"
            );
        }
        assert_eq!(
            a.usage_stats(),
            b.usage_stats(),
            "usage calendar buckets must match after restore (usage-stats)"
        );
    }

    // ---- usage calendar buckets (usage-stats) ----

    /// A finished, attributed request with NO tokens (pre-completion failure):
    /// must still count in the usage requests column.
    fn finished_tokenless(id: u64, group: &str, model: &str) -> ActivityEvent {
        ActivityEvent::RequestFinished {
            id,
            method: "POST".into(),
            path: "/v1/messages".into(),
            account: Some("acct".into()),
            status: 500,
            duration: Duration::from_millis(10),
            tokens: None,
            group: Some(group.into()),
            model: Some(model.into()),
            effort: None,
            fast: false,
            user_id: None,
            kind: None,
            excerpt: None,
        }
    }

    /// Rows of one granularity, in document order (newest bucket first).
    fn usage_rows(log: &ActivityLog, gran: UsageGran) -> Vec<crate::dashboard::UsageStatDoc> {
        log.usage_stats()
            .into_iter()
            .filter(|r| r.gran == gran.tag())
            .collect()
    }

    /// 2024-01-01 is epoch day 19_723 (pinned by `format::civil_from_days`
    /// tests). All usage tests bucket at a fixed +9h offset (KST — the
    /// interesting case: local day ≠ UTC day for evening traffic).
    const JAN1_2024: u64 = 19_723;
    const KST: i64 = 9 * 3600;

    fn at_day_hm(day: u64, hour: u64, minute: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(day * 86_400 + hour * 3_600 + minute * 60)
    }

    #[test]
    fn usage_buckets_hour_day_month_at_local_offset() {
        let mut log = ActivityLog::new(LOG_CAPACITY);
        log.set_usage_offset(KST);
        // 23:30 UTC Jan 1 → 08:30 KST Jan 2: local day rolls over.
        log.apply(
            finished_full(
                1,
                "a",
                "claude",
                "m-late",
                None,
                200,
                100,
                10,
                Some(7),
                "/v1/messages",
            ),
            at_day_hm(JAN1_2024, 23, 30),
        );
        // 10:00 UTC Jan 1 → 19:00 KST Jan 1: same local day.
        log.apply(
            finished_full(
                2,
                "a",
                "claude",
                "m-noon",
                None,
                200,
                200,
                20,
                None,
                "/v1/messages",
            ),
            at_day_hm(JAN1_2024, 10, 0),
        );

        let hours = usage_rows(&log, UsageGran::Hour);
        assert_eq!(hours.len(), 2, "one hour bucket per event");
        // Newest first: the 23:00 UTC bucket leads, labeled in LOCAL wall
        // clock (08h on Jan 2).
        assert_eq!(hours[0].bucket, JAN1_2024 * 24 + 23);
        assert_eq!(hours[0].label, "01-02 08h");
        assert_eq!(hours[0].model, "m-late");
        assert_eq!(
            (hours[0].tokens_in, hours[0].tokens_out, hours[0].cache_read),
            (100, 10, 7)
        );
        assert_eq!(hours[1].label, "01-01 19h");

        let days = usage_rows(&log, UsageGran::Day);
        assert_eq!(
            days.len(),
            2,
            "the evening event lands on the NEXT local day"
        );
        assert_eq!(days[0].bucket, JAN1_2024 + 1);
        assert_eq!(days[0].label, "2024-01-02");
        assert_eq!(days[0].model, "m-late");
        assert_eq!(days[1].label, "2024-01-01");
        assert_eq!(days[1].model, "m-noon");

        let months = usage_rows(&log, UsageGran::Month);
        assert_eq!(months.len(), 2, "one month bucket, two model rows");
        assert!(months.iter().all(|r| r.label == "2024-01"));
        assert!(months.iter().all(|r| r.bucket == 2024 * 12));
    }

    #[test]
    fn usage_month_year_rollover_follows_local_calendar() {
        let mut log = ActivityLog::new(LOG_CAPACITY);
        log.set_usage_offset(KST);
        // 2023-12-31 23:00 UTC → 2024-01-01 08:00 KST: the LOCAL month is
        // January even though the UTC month is December.
        log.apply(
            finished_full(1, "a", "claude", "m", None, 200, 1, 1, None, "/v1/messages"),
            at_day_hm(JAN1_2024 - 1, 23, 0),
        );
        let months = usage_rows(&log, UsageGran::Month);
        assert_eq!(months.len(), 1);
        assert_eq!(months[0].label, "2024-01");
        let days = usage_rows(&log, UsageGran::Day);
        assert_eq!(days[0].label, "2024-01-01");
    }

    #[test]
    fn usage_counts_tokenless_requests_and_skips_unattributed() {
        let mut log = ActivityLog::new(LOG_CAPACITY);
        log.set_usage_offset(0);
        log.apply(
            finished_tokenless(1, "codex", "gpt-5.5"),
            at_day_hm(JAN1_2024, 1, 0),
        );
        // Unattributed (no group/model) request: stays out of usage buckets,
        // same rule as the per-model rows.
        log.apply(
            finished(2, Some("acct"), Some((5, 5))),
            at_day_hm(JAN1_2024, 1, 5),
        );
        let hours = usage_rows(&log, UsageGran::Hour);
        assert_eq!(hours.len(), 1);
        assert_eq!(hours[0].requests, 1, "tokenless request still counted");
        assert_eq!(hours[0].tokens_in + hours[0].tokens_out, 0);
    }

    #[test]
    fn usage_hourly_and_daily_retention_monthly_unbounded() {
        let mut log = ActivityLog::new(LOG_CAPACITY);
        log.set_usage_offset(0);
        log.apply(
            finished_full(1, "a", "claude", "m", None, 200, 1, 1, None, "/p"),
            at_day_hm(JAN1_2024, 0, 0),
        );
        // 200 days later: outside both the 72h hourly and the 180d daily
        // retention of the FIRST event.
        log.apply(
            finished_full(2, "a", "claude", "m", None, 200, 1, 1, None, "/p"),
            at_day_hm(JAN1_2024 + 200, 0, 0),
        );
        assert_eq!(
            usage_rows(&log, UsageGran::Hour).len(),
            1,
            "old hour pruned"
        );
        assert_eq!(usage_rows(&log, UsageGran::Day).len(), 1, "old day pruned");
        assert_eq!(
            usage_rows(&log, UsageGran::Month).len(),
            2,
            "months are never pruned"
        );
    }

    #[test]
    fn persisted_round_trip_rebuilds_identical_aggregates() {
        let tmp = TempDir::new();
        let path = tmp.file();
        let events = vec![
            (
                finished_full(
                    1,
                    "a",
                    "claude",
                    "claude-sonnet-4-5[1m]",
                    Some("16k"),
                    200,
                    700,
                    300,
                    Some(900),
                    "/v1/messages",
                ),
                at(10),
            ),
            (
                finished_full(
                    2,
                    "b",
                    "codex",
                    "gpt-5.5",
                    Some("high"),
                    200,
                    50,
                    20,
                    None,
                    "/v1/messages",
                ),
                at(20),
            ),
            (
                finished_full(
                    3,
                    "a",
                    "claude",
                    "claude-sonnet-4-5",
                    None,
                    529,
                    0,
                    0,
                    None,
                    "/v1/messages/count_tokens",
                ),
                at(30),
            ),
        ];

        let live = live_log(&events);
        let restored = persisted_then_loaded(&path, &events);

        assert_same_aggregates(&live, &restored, &["a", "b", "ghost"]);
        // Sanity: the restore is non-trivial (two model rows, three requests).
        assert_eq!(restored.model_usage().len(), 2);
        assert_eq!(restored.totals_global().requests, 3);
    }

    #[test]
    fn stats_continue_cumulatively_across_a_restart() {
        let tmp = TempDir::new();
        let path = tmp.file();

        // Session 1: N events, persisted as they fold.
        let session1 = vec![
            (
                finished_full(
                    1,
                    "a",
                    "claude",
                    "sonnet",
                    Some("16k"),
                    200,
                    100,
                    40,
                    Some(10),
                    "/v1/messages",
                ),
                at(10),
            ),
            (
                finished_full(
                    2,
                    "a",
                    "claude",
                    "sonnet",
                    None,
                    200,
                    200,
                    60,
                    None,
                    "/v1/messages",
                ),
                at(20),
            ),
        ];
        {
            let mut log1 = ActivityLog::new(LOG_CAPACITY);
            for (event, ts) in &session1 {
                persist_request(Some(&path), event, *ts);
                log1.apply(event.clone(), *ts);
            }
            // log1 dropped here — simulates daemon restart.
        }

        // Session 2: load the persisted log (resume), then M more events.
        let mut log2 = ActivityLog::new(LOG_CAPACITY);
        log2.load_persisted(Some(&path));
        let session2 = vec![
            (
                finished_full(
                    3,
                    "a",
                    "claude",
                    "sonnet",
                    None,
                    200,
                    300,
                    90,
                    None,
                    "/v1/messages",
                ),
                at(30),
            ),
            (
                finished_full(
                    4,
                    "b",
                    "codex",
                    "gpt-5.5",
                    None,
                    200,
                    5,
                    5,
                    None,
                    "/v1/messages",
                ),
                at(40),
            ),
        ];
        for (event, ts) in &session2 {
            log2.apply(event.clone(), *ts);
        }

        // Totals must equal ALL N+M events, not reset to just session 2.
        let mut all = session1.clone();
        all.extend(session2.clone());
        let oracle = live_log(&all);
        assert_same_aggregates(&oracle, &log2, &["a", "b"]);
        assert_eq!(
            log2.totals_global().requests,
            4,
            "stats continue, not reset"
        );
        // Account a: 3 requests, 600 in / 190 out across both sessions.
        assert_eq!(log2.totals_for("a").requests, 3);
        assert_eq!(log2.totals_for("a").tokens_in, 600);
        assert_eq!(log2.totals_for("a").tokens_out, 190);
    }

    #[test]
    fn corrupt_and_old_lines_are_tolerated() {
        let tmp = TempDir::new();
        let path = tmp.file();

        // A valid line (write it through the real persist path).
        persist_request(
            Some(&path),
            &finished_full(
                1,
                "a",
                "claude",
                "sonnet",
                None,
                200,
                10,
                5,
                None,
                "/v1/messages",
            ),
            at(10),
        );
        // Append garbage + a blank line + a wrong-version line by hand.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("reopen");
            writeln!(f, "this is not json {{").expect("write garbage");
            writeln!(f).expect("write blank");
            // Structurally valid JSON but a future/unknown schema version.
            writeln!(
                f,
                r#"{{"v":99,"ts_ms":1,"id":7,"method":"POST","path":"/x","account":null,"status":200,"duration_ms":1,"tokens":null,"group":null,"model":null,"effort":null}}"#
            )
            .expect("write old-version");
        }
        // Another valid line after the junk.
        persist_request(
            Some(&path),
            &finished_full(
                2,
                "b",
                "codex",
                "gpt-5.5",
                None,
                200,
                20,
                8,
                None,
                "/v1/messages",
            ),
            at(20),
        );

        let mut log = ActivityLog::new(LOG_CAPACITY);
        log.load_persisted(Some(&path)); // must not panic
                                         // Only the two valid lines loaded; garbage + v99 skipped.
        assert_eq!(log.totals_global().requests, 2);
        assert_eq!(log.totals_for("a").requests, 1);
        assert_eq!(log.totals_for("b").requests, 1);
        assert!(
            !log.model_usage().iter().any(|m| m.requests == 0),
            "no phantom row from the skipped v99 line"
        );
    }

    #[test]
    fn persistence_is_best_effort_none_and_unwritable_paths_never_panic() {
        // None path: persist + load are silent no-ops, fold still works.
        let mut log = ActivityLog::new(LOG_CAPACITY);
        let event = finished_full(
            1,
            "a",
            "claude",
            "sonnet",
            None,
            200,
            10,
            5,
            None,
            "/v1/messages",
        );
        persist_request(None, &event, at(10)); // no panic, nothing written
        log.load_persisted(None); // no panic, no-op
        log.apply(event, at(10)); // in-memory fold unaffected
        assert_eq!(log.totals_global().requests, 1);

        // Unwritable path: the parent is a *file*, so create_dir_all + open
        // both fail — swallowed, no panic, in-memory state untouched.
        let tmp = TempDir::new();
        let blocker = tmp.0.join("not-a-dir");
        std::fs::write(&blocker, b"x").expect("seed blocker file");
        let bad = blocker.join("activity.jsonl"); // parent is a file
        let mut log2 = ActivityLog::new(LOG_CAPACITY);
        persist_request(
            Some(&bad),
            &finished_full(
                2,
                "a",
                "claude",
                "sonnet",
                None,
                200,
                1,
                1,
                None,
                "/v1/messages",
            ),
            at(20),
        );
        log2.load_persisted(Some(&bad)); // read fails → no-op, no panic
        assert_eq!(
            log2.totals_global().requests,
            0,
            "unwritable path wrote nothing"
        );
    }

    /// Background hydration must equal the old blocking replay: a live log
    /// that folded traffic FIRST and merged history BEHIND it afterwards
    /// carries the same aggregates, ring order, and windowed buckets as a log
    /// that replayed history before the live events (the pre-lazy behavior).
    /// History sits 30h before live so the 24h window would EXPOSE misplaced
    /// buckets: folding old events through `apply` after live traffic would
    /// dump them into the current hour (`roll_to` never rewinds) and
    /// contaminate the 24h heatmap.
    #[test]
    fn merge_history_behind_matches_blocking_replay_and_keeps_hours() {
        let tmp = TempDir::new();
        let path = tmp.file();
        let history_events = vec![
            (
                finished_full(
                    1,
                    "a",
                    "claude",
                    "sonnet",
                    Some("16k"),
                    200,
                    100,
                    40,
                    Some(10),
                    "/v1/messages",
                ),
                at(10),
            ),
            (
                finished_full(
                    2,
                    "b",
                    "codex",
                    "gpt-5.5",
                    None,
                    529,
                    0,
                    0,
                    None,
                    "/v1/messages/count_tokens",
                ),
                at(20),
            ),
        ];
        let now = at(30 * 3600); // live traffic 30h later
        let live_events = vec![
            (
                finished_full(
                    3,
                    "a",
                    "claude",
                    "sonnet",
                    None,
                    200,
                    300,
                    90,
                    None,
                    "/v1/messages",
                ),
                now,
            ),
            (
                finished_full(
                    4,
                    "c",
                    "claude",
                    "opus",
                    None,
                    200,
                    5,
                    5,
                    None,
                    "/v1/messages",
                ),
                now,
            ),
        ];

        // Oracle: the old blocking order — history replayed first, live after.
        let mut all = history_events.clone();
        all.extend(live_events.clone());
        let oracle = live_log(&all);

        // Lazy path: live folds first; history replays from disk into a fresh
        // log and merges behind.
        let merged = {
            let mut live = live_log(&live_events);
            let history = persisted_then_loaded(&path, &history_events);
            let n = live.merge_history_behind(history);
            assert_eq!(n, 2, "reports the number of merged historical requests");
            live
        };

        assert_same_aggregates(&oracle, &merged, &["a", "b", "c"]);
        assert_eq!(
            oracle.completed().cloned().collect::<Vec<_>>(),
            merged.completed().cloned().collect::<Vec<_>>(),
            "ring order: live rows in front, history behind"
        );
        for window in StatsWindow::ALL {
            assert_eq!(
                oracle.windowed_rows(window, now),
                merged.windowed_rows(window, now),
                "{} heatmap: history lands in its ORIGINAL hours",
                window.label()
            );
        }
        // The guard the equality above encodes: 30h-old history is visible in
        // the 72h window but absent from the 24h one.
        assert!(
            !merged
                .windowed_rows(StatsWindow::Day, now)
                .iter()
                .any(|r| r.account == "b"),
            "24h window excludes 30h-old history"
        );
        assert!(
            merged
                .windowed_rows(StatsWindow::ThreeDay, now)
                .iter()
                .any(|r| r.account == "b"),
            "72h window includes it"
        );
    }

    /// A live in-flight request whose id collides with a historical record
    /// (activity ids restart at boot) must survive hydration: the merge never
    /// routes history through `apply`, so the replayed finish can't swallow
    /// the live row.
    #[test]
    fn merge_history_behind_never_touches_live_in_flight() {
        let tmp = TempDir::new();
        let path = tmp.file();
        let history = persisted_then_loaded(
            &path,
            &[(
                finished_full(
                    7,
                    "a",
                    "claude",
                    "sonnet",
                    None,
                    200,
                    10,
                    5,
                    None,
                    "/v1/messages",
                ),
                at(10),
            )],
        );

        let now = at(1000);
        let mut live = ActivityLog::new(LOG_CAPACITY);
        live.apply(started(7), now); // same id as the historical record
        live.merge_history_behind(history);

        assert_eq!(live.in_flight().len(), 1, "live in-flight row survives");
        assert_eq!(live.in_flight()[0].id, 7);
        assert_eq!(
            live.totals_global().requests,
            1,
            "history still counted exactly once"
        );
    }

    /// The hydration cut: only bytes before `up_to` are replayed, so a live
    /// request appended to the same file DURING hydration (it is past the cut
    /// and already folded live) is never double-counted.
    #[test]
    fn load_persisted_prefix_stops_at_the_cut() {
        let tmp = TempDir::new();
        let path = tmp.file();
        persist_request(
            Some(&path),
            &finished_full(
                1,
                "a",
                "claude",
                "sonnet",
                None,
                200,
                10,
                5,
                None,
                "/v1/messages",
            ),
            at(10),
        );
        let cut = std::fs::metadata(&path).expect("metadata").len();
        // A live append lands past the cut while "hydration" is in flight.
        persist_request(
            Some(&path),
            &finished_full(
                2,
                "b",
                "codex",
                "gpt-5.5",
                None,
                200,
                20,
                8,
                None,
                "/v1/messages",
            ),
            at(20),
        );

        let mut log = ActivityLog::new(LOG_CAPACITY);
        log.load_persisted_prefix(&path, cut).expect("readable");
        assert_eq!(log.totals_global().requests, 1, "only pre-cut history");
        assert_eq!(log.totals_for("a").requests, 1);
        assert_eq!(
            log.totals_for("b").requests,
            0,
            "post-cut line not replayed"
        );
    }
}
