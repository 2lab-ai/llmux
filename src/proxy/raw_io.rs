//! Raw input/output payload capture (`[raw_io]`, Feature B): one JSON line per
//! proxied request appended to `$XDG_STATE_HOME/llmux/raw-io.jsonl`, holding the
//! verbatim request body and the response body delivered to the client, so the
//! actual traffic can be replayed/audited offline.
//!
//! This is DISTINCT from activity persistence (`activity.jsonl`), which keeps
//! per-request *metadata* (status, tokens, model). This store keeps the payload
//! *bytes*.
//!
//! # Best-effort, never on the hot path
//!
//! Capture mirrors the discipline in [`crate::proxy::codex_trace`]: building a
//! record never fails the request, and [`append`] swallows every IO/
//! serialization error. The proxy must never block, backpressure, or mutate the
//! bytes forwarded to the client to capture them — so the response body is only
//! ever an *observed copy*, filled on the relay's pump task AFTER each chunk has
//! been forwarded to the client (`tx.send` first, the copy is a side effect),
//! written when the client stream has finished. A disabled config or an
//! unresolvable state dir makes the whole thing a no-op.
//!
//! # Decoupled from the 8 KiB debug body-log cap
//!
//! The streaming relays keep TWO independent observe-only buffers: a short one
//! capped at the debug request-log's 8 KiB
//! [`crate::proxy::logging::BODY_LOG_LIMIT`] (for the `=== RESPONSE BODY ===`
//! log excerpt) and a separate one for raw-io capped at the configurable
//! [`crate::config::RawIoConfig::max_body_bytes`] (default
//! [`RESPONSE_CAP_BYTES`], 8 MiB). They are filled side by side from the same
//! forwarded chunks. Reusing the 8 KiB debug cap for raw-io would truncate
//! every streamed response to 8 KiB — but real LLM responses stream tens to
//! hundreds of KB, so raw-io needs its own, much larger cap to retain the full
//! payload the feature exists to keep.
//!
//! # Memory cost (the intended tradeoff)
//!
//! With its own buffer, each in-flight STREAMED request can pin up to
//! `max_body_bytes` of response bytes (plus the request body, itself bounded by
//! the same cap) until the stream finishes and the record is flushed. That
//! memory is bounded by the proxy's concurrency cap × `max_body_bytes`. This is
//! the deliberate price of full-payload retention; tune `max_body_bytes` down if
//! the ceiling is too high for the host.
//!
//! # What is captured on each path
//!
//! - **Non-streaming** (`relay` JSON path): request body + the full response
//!   body (it is already materialized to relay it).
//! - **Codex** (`relay_codex`, streaming and non-streaming): request body + the
//!   bytes EMITTED to the client (the converter's Anthropic-SSE output for
//!   streaming clients, the aggregated Messages JSON for non-streaming),
//!   bounded by [`RESPONSE_CAP_BYTES`] / the relay's own capture limit.
//! - **Claude streaming passthrough** (`relay` SSE path): request body + a
//!   BOUNDED tee of the SSE bytes streamed to the client. The tee is a dedicated
//!   raw-io buffer (`passthrough_body`'s `raw_capture_limit`), an in-memory `Vec`
//!   capped at `max_body_bytes`, filled on the pump task AFTER each chunk is
//!   forwarded — it never blocks, slows, or alters the chunk sent to the client
//!   (the chunk is `tx.send`'d first; the copy is a side effect). The record is
//!   flushed in the relay's `finish` closure, after the client stream completes
//!   or on disconnect/error with whatever was captured so far.
//!
//! # Bounds
//!
//! Each captured body is clipped to the configurable `max_body_bytes`
//! ([`crate::config::RawIoConfig::max_body_bytes`], default
//! [`RESPONSE_CAP_BYTES`]) on a UTF-8 char boundary with a
//! `…[truncated N bytes]` marker, so a pathological huge body can't blow memory.
//! The streaming relays accumulate their dedicated raw-io tee up to this same
//! cap (then stop growing), and the non-streaming full-body path clips to it as
//! the final backstop — one cap, every path, request and response alike.

use std::io::{BufRead as _, Seek as _, Write as _};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Serializes [`append`] against the commit step of a concurrent [`prune`].
/// Since prune runs in the background after the listener is up (it no longer
/// blocks readiness), live requests can append while a prune is rewriting the
/// file; the commit holds this lock while copying the appended tail onto the
/// pruned temp file and renaming it into place, so no live record is lost.
/// Appends are short (one buffered line) — contention is negligible.
static IO_LOCK: Mutex<()> = Mutex::new(());

/// Schema version of a [`RawIoRecord`] line. Bump on a breaking layout change;
/// [`prune`] tolerates (skips) lines it cannot parse, so old/new lines coexist.
pub const RECORD_VERSION: u8 = 1;

/// Default cap on each captured body before it is stored, used when no
/// per-config [`crate::config::RawIoConfig::max_body_bytes`] is supplied. A body
/// over the effective cap is clipped on a char boundary with a
/// `…[truncated N bytes]` marker. 8 MiB is generous for a real request/response
/// yet bounds the memory a pathological body can pin.
///
/// This is DELIBERATELY larger than, and independent of, the debug request
/// log's 8 KiB [`crate::proxy::logging::BODY_LOG_LIMIT`]: the debug log keeps a
/// short excerpt for eyeballing while raw-io retains the full (bounded) body for
/// replay/audit. Most LLM responses stream tens to hundreds of KB, so reusing
/// the 8 KiB debug cap here would discard almost the entire response.
pub const RESPONSE_CAP_BYTES: usize = 8 * 1024 * 1024;

/// Milliseconds in a day, for the retention window arithmetic.
const MS_PER_DAY: u64 = 86_400_000;

/// One raw-io line: the verbatim request/response payloads for a single proxied
/// request plus the correlation/attribution fields the forward path already
/// knows. Field-named JSON so adding a field stays backward-readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawIoRecord {
    /// Schema version ([`RECORD_VERSION`]).
    pub v: u8,
    /// Capture timestamp, millis since the Unix epoch (the retention key).
    pub ts_ms: u64,
    /// The request's activity id (correlates with `activity.jsonl` /
    /// `codex-trace.jsonl` / the dashboard feed).
    pub id: u64,
    /// Backend group served ("claude"/"codex"), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Model served, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Account name that served the request, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// HTTP status delivered to the client, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Total request duration, millis — lets the session timeline derive an
    /// honest Σoutput/Σduration throughput (perf telemetry v1). Additive:
    /// `None` on records written before the field existed (those requests
    /// simply don't contribute to the session rate sums).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Verbatim request body (bounded + truncation-marked at capture time).
    pub request_body: String,
    /// Response body delivered to the client (bounded + truncation-marked).
    pub response_body: String,
    /// Inbound client request headers, in wire order, SENSITIVE VALUES
    /// REDACTED at capture time (see `forward::redacted_header_pairs`).
    /// Additive: `None` on records written before headers were captured —
    /// the raw viewer renders that as "not captured", distinct from `Some([])`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<Vec<(String, String)>>,
    /// Response headers delivered to the client (upstream's, post-sanitize),
    /// same redaction + additive semantics as `request_headers`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<Vec<(String, String)>>,
    /// The upstream (proxy→API) half of a TRANSLATED exchange (codex/grok):
    /// the rewritten Responses-API request the proxy actually sent and the
    /// verbatim upstream reply it transformed for the client. `None` on the
    /// byte-identity claude passthrough — client and upstream payloads are the
    /// same bytes, so the raw viewer renders 2 payloads instead of 4.
    /// Additive: records written before this field load as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<UpstreamRaw>,
}

/// The proxy→API side of one translated exchange (see
/// [`RawIoRecord::upstream`]). Bodies arrive PRE-BOUNDED by the caller (the
/// forward path applies the same `max_body_bytes` cap + truncation marker as
/// the client-side bodies); headers arrive pre-redacted
/// (`forward::redacted_header_pairs`) so upstream bearer tokens never land on
/// disk. Every field is optional — an upstream error path may know the request
/// half but have no transformed response, and vice versa.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamRaw {
    /// Full upstream URL (`endpoint` + provider path), the `copy as curl`
    /// target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Rewritten request body the proxy sent upstream (bounded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    /// Upstream request headers (wire order, credential values redacted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<Vec<(String, String)>>,
    /// Verbatim upstream response body BEFORE transformation (bounded; the
    /// Responses-API SSE/JSON the converter consumed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    /// Upstream response headers (redacted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<Vec<(String, String)>>,
}

/// Captured header pairs (wire order, values already redacted by the caller);
/// `None` = headers not captured (pre-headers record or caller had none).
pub type HeaderPairs = Option<Vec<(String, String)>>;

impl RawIoRecord {
    /// Build a record from raw bytes, clipping each body to `max_body_bytes`
    /// (the configurable raw-io cap; see
    /// [`crate::config::RawIoConfig::max_body_bytes`], default
    /// [`RESPONSE_CAP_BYTES`]). The SAME cap applies to request and response.
    /// `now_ms` is the capture timestamp; the bodies are stored as lossy UTF-8
    /// (binary payloads degrade gracefully, never panic).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        now_ms: u64,
        group: Option<String>,
        model: Option<String>,
        account: Option<String>,
        status: Option<u16>,
        request_body: &[u8],
        response_body: &[u8],
        max_body_bytes: usize,
        request_headers: HeaderPairs,
        response_headers: HeaderPairs,
        upstream: Option<UpstreamRaw>,
    ) -> Self {
        Self {
            v: RECORD_VERSION,
            ts_ms: now_ms,
            id,
            group,
            model,
            account,
            status,
            duration_ms: None,
            request_body: bounded_body(request_body, max_body_bytes),
            response_body: bounded_body(response_body, max_body_bytes),
            request_headers,
            response_headers,
            upstream,
        }
    }
}

/// Clip a body to `max_body_bytes` on a UTF-8 char boundary, appending a
/// `…[truncated N bytes]` marker when it overflows. A body within the cap is
/// returned whole (lossy UTF-8). Pure; never panics. `pub(crate)` so the
/// forward path can pre-bound [`UpstreamRaw`] bodies with the SAME cap +
/// marker as the client-side bodies.
pub(crate) fn bounded_body(body: &[u8], max_body_bytes: usize) -> String {
    let s = String::from_utf8_lossy(body);
    if s.len() <= max_body_bytes {
        return s.into_owned();
    }
    let mut end = max_body_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = s.len() - end;
    format!("{}…[truncated {} bytes]", &s[..end], dropped)
}

/// Render a STREAMED body that was already bounded at capture time: `kept` is
/// the retained prefix (capped at the relay's raw-io limit) and `total` is the
/// full number of bytes that streamed past the tee. When `total > kept.len()`
/// the body overflowed the cap, so we append the same `…[truncated N bytes]`
/// marker with the exact dropped count — which `bounded_body` alone cannot
/// compute, because the relay only handed us the bounded prefix, not the whole
/// body. When nothing was dropped the prefix is returned whole (lossy UTF-8).
/// Pure; never panics. `pub(crate)` for the forward path's upstream tee (same
/// bounded-prefix + total shape as the client-side stream tee).
pub(crate) fn bounded_body_streamed(kept: &[u8], total: usize) -> String {
    let s = String::from_utf8_lossy(kept).into_owned();
    let dropped = total.saturating_sub(kept.len());
    if dropped == 0 {
        return s;
    }
    format!("{s}…[truncated {dropped} bytes]")
}

/// Wall-clock now, millis since the Unix epoch. Mirrors the idiom in
/// `tui::activity` / `forward`.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Append one record as a JSON line to `path`, best-effort. A `None` path (no
/// state dir / disabled), a serialization failure, or any IO error is swallowed
/// — the request path is never affected, nothing here panics. The parent dir is
/// created if missing; the file is opened `create(true).append(true)`.
pub fn append(path: Option<&std::path::Path>, record: &RawIoRecord) {
    let Some(path) = path else {
        return;
    };
    let Ok(line) = serde_json::to_string(record) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Serialized against a background prune's commit (see [`IO_LOCK`]): an
    // append lands either before the commit's tail copy (and is carried over)
    // or after the rename (and lands in the pruned file) — never in between.
    let _guard = IO_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let _ = writeln!(file, "{line}");
}

/// Build a record at `now_ms()` and [`append`] it, best-effort. The single
/// entry point the forward path calls at a request's terminal outcome: a
/// `None` path (capture disabled / no state dir) is a silent no-op, so callers
/// need not branch. The bodies are clipped to `max_body_bytes` (the
/// configurable raw-io cap; default [`RESPONSE_CAP_BYTES`]).
#[allow(clippy::too_many_arguments)]
pub fn capture(
    path: Option<&std::path::Path>,
    id: u64,
    group: Option<String>,
    model: Option<String>,
    account: Option<String>,
    status: Option<u16>,
    duration_ms: Option<u64>,
    request_body: &[u8],
    response_body: &[u8],
    max_body_bytes: usize,
    request_headers: HeaderPairs,
    response_headers: HeaderPairs,
    upstream: Option<UpstreamRaw>,
) {
    if path.is_none() {
        return; // disabled / no state dir — skip building the record at all
    }
    let mut record = RawIoRecord::new(
        id,
        now_ms(),
        group,
        model,
        account,
        status,
        request_body,
        response_body,
        max_body_bytes,
        request_headers,
        response_headers,
        upstream,
    );
    record.duration_ms = duration_ms;
    append(path, &record);
}

impl RawIoRecord {
    /// Build a record for a STREAMED response: the request body is clipped to
    /// `max_body_bytes` as usual, but the response is the relay's raw-io tee —
    /// already bounded at capture time to `response_kept` with `response_total`
    /// total bytes seen — so its truncation marker is computed from the dropped
    /// count the relay observed (see [`bounded_body_streamed`]). This is what
    /// lets a streamed body that overflows the cap carry an accurate
    /// `…[truncated N bytes]` marker even though only the bounded prefix reaches
    /// this point.
    #[allow(clippy::too_many_arguments)]
    pub fn new_streamed(
        id: u64,
        now_ms: u64,
        group: Option<String>,
        model: Option<String>,
        account: Option<String>,
        status: Option<u16>,
        request_body: &[u8],
        response_kept: &[u8],
        response_total: usize,
        max_body_bytes: usize,
        request_headers: HeaderPairs,
        response_headers: HeaderPairs,
        upstream: Option<UpstreamRaw>,
    ) -> Self {
        Self {
            v: RECORD_VERSION,
            ts_ms: now_ms,
            id,
            group,
            model,
            account,
            status,
            duration_ms: None,
            request_body: bounded_body(request_body, max_body_bytes),
            response_body: bounded_body_streamed(response_kept, response_total),
            request_headers,
            response_headers,
            upstream,
        }
    }
}

/// Streaming sibling of [`capture`]: build a record at `now_ms()` from the
/// relay's raw-io tee (`response_kept` = retained prefix, `response_total` =
/// full streamed length) and [`append`] it, best-effort. A `None` path is a
/// silent no-op. The request body is clipped to `max_body_bytes`; the response
/// is marker-truncated from the relay's observed dropped count.
#[allow(clippy::too_many_arguments)]
pub fn capture_streamed(
    path: Option<&std::path::Path>,
    id: u64,
    group: Option<String>,
    model: Option<String>,
    account: Option<String>,
    status: Option<u16>,
    duration_ms: Option<u64>,
    request_body: &[u8],
    response_kept: &[u8],
    response_total: usize,
    max_body_bytes: usize,
    request_headers: HeaderPairs,
    response_headers: HeaderPairs,
    upstream: Option<UpstreamRaw>,
) {
    if path.is_none() {
        return; // disabled / no state dir — skip building the record at all
    }
    let mut record = RawIoRecord::new_streamed(
        id,
        now_ms(),
        group,
        model,
        account,
        status,
        request_body,
        response_kept,
        response_total,
        max_body_bytes,
        request_headers,
        response_headers,
        upstream,
    );
    record.duration_ms = duration_ms;
    append(path, &record);
}

/// How far a record's capture timestamp may sit from the activity entry's
/// completion timestamp and still be "the same request" ([`find_record`]).
/// Capture and finish happen on the same host within milliseconds of each
/// other; 5 minutes absorbs any fold/flush lag while keeping id collisions
/// from OTHER daemon runs (the activity id is a per-process counter that
/// resets on restart) out of the match.
const FIND_TS_WINDOW_MS: u64 = 300_000;

/// Find the raw-io record for activity `id` completed around `at_ms`, reading
/// the log BACKWARDS from the end (clicks land on recent entries; the file can
/// be tens of GB, so a forward scan would read all of it for every lookup).
///
/// Among the in-window id matches the one with the CLOSEST capture timestamp
/// to `at_ms` wins — never the first (= newest) match. The id alone is NOT
/// unique across daemon restarts (per-process counter), and two runs less
/// than the window apart can BOTH have an in-window record for the same id;
/// capture and completion are stamped at the same terminal moment on the same
/// host, so the true record sits within fold-lag milliseconds of `at_ms` and
/// min-distance selection is effectively exact (trinity review R2, gpt-5.6
/// MUST-FIX 1). Records are appended in capture-time order, so the scan
/// early-exits once it walks past `at_ms - FIND_TS_WINDOW_MS` (or, with a
/// candidate in hand, past the point where anything older must lose). Corrupt/
/// foreign lines are skipped. Strictly best-effort and read-only: any IO
/// error yields `None` (or the best candidate so far), never a panic.
pub fn find_record(path: Option<&std::path::Path>, id: u64, at_ms: u64) -> Option<RawIoRecord> {
    let path = path?;
    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    // Cheap pre-filter fragment: a matching line MUST contain `"id":<id>,`
    // (field-named JSON, serde writes no spaces). Avoids parsing every line.
    let needle = format!("\"id\":{id},");
    let floor = at_ms.saturating_sub(FIND_TS_WINDOW_MS);
    let ceil = at_ms.saturating_add(FIND_TS_WINDOW_MS);
    let mut best: Option<(u64, RawIoRecord)> = None; // (distance to at_ms, record)
    let keep_best = |candidate: RawIoRecord, best: &mut Option<(u64, RawIoRecord)>| {
        let dist = candidate.ts_ms.abs_diff(at_ms);
        if best.as_ref().is_none_or(|(d, _)| dist < *d) {
            *best = Some((dist, candidate));
        }
    };

    const CHUNK: u64 = 256 * 1024;
    let mut reader = file;
    let mut pos = len;
    // Bytes of the (possibly partial) line carried over from the newer chunk.
    let mut carry: Vec<u8> = Vec::new();
    while pos > 0 {
        let take = CHUNK.min(pos);
        pos -= take;
        let mut buf = vec![0u8; usize::try_from(take).ok()?];
        reader.seek(std::io::SeekFrom::Start(pos)).ok()?;
        std::io::Read::read_exact(&mut reader, &mut buf).ok()?;
        buf.extend_from_slice(&carry);
        // Everything before the FIRST newline may be a partial line whose head
        // lives in the next-older chunk — carry it over (unless at file start).
        let first_nl = buf.iter().position(|&b| b == b'\n');
        let (head, complete) = match first_nl {
            Some(i) if pos > 0 => buf.split_at(i + 1),
            _ => (&[][..], &buf[..]),
        };
        for line in complete.split(|&b| b == b'\n').rev() {
            if line.is_empty() {
                continue;
            }
            let Ok(text) = std::str::from_utf8(line) else {
                continue;
            };
            if !text.contains(&needle) {
                continue;
            }
            let Ok(record) = serde_json::from_str::<RawIoRecord>(text.trim()) else {
                continue;
            };
            if record.id != id || record.v != RECORD_VERSION {
                continue;
            }
            let ts = record.ts_ms;
            if ts >= floor && ts <= ceil {
                keep_best(record, &mut best);
                // Scanning backwards, timestamps only get older: once we are
                // AT/BELOW at_ms with a candidate this close, nothing older
                // can sit closer — done.
                if let Some((dist, _)) = &best {
                    if ts <= at_ms && at_ms.abs_diff(ts) >= *dist {
                        return best.map(|(_, r)| r);
                    }
                }
                continue;
            }
            if ts < floor {
                // Append order: everything further back is older still.
                return best.map(|(_, r)| r);
            }
        }
        // Time-based early exit even when no id matched in this chunk: parse
        // the OLDEST complete line here (one parse per 256 KiB); if it is
        // already before the window, everything further back is older still.
        if let Some(oldest) = complete
            .split(|&b| b == b'\n')
            .find(|l| !l.is_empty())
            .and_then(|l| std::str::from_utf8(l).ok())
            .and_then(|t| serde_json::from_str::<RawIoRecord>(t.trim()).ok())
        {
            if oldest.ts_ms < floor {
                return best.map(|(_, r)| r);
            }
        }
        carry = head.to_vec();
    }
    best.map(|(_, r)| r)
}

/// Prune the raw-io log to its lifetime contract (issue #127), best-effort.
///
/// Two independent bounds, either of which can trigger a rewrite:
///
/// - **Age** — when `retention_days > 0`, records whose
///   `ts_ms < now_ms - retention_days * 86_400_000` are dropped. `0` = no age
///   limit.
/// - **Size** — when `max_total_bytes > 0` and the file is larger, the OLDEST
///   records are dropped (whole lines, front of the file — appends are in
///   capture-time order) until the kept set fits. `0` = no size cap. The cap
///   is approximate under concurrent appends: bytes landing during the pass
///   are always kept, so the file can transiently exceed the cap until the
///   next pass.
///
/// Corrupt lines (not JSON, or not the current [`RECORD_VERSION`]) are
/// tolerated by being DROPPED — a rewrite is a natural point to shed
/// unreadable history; the kept set is exactly the in-window, parseable
/// records.
///
/// # Streaming + safe against concurrent appends
///
/// The log can be tens of GB (one line per proxied request, bodies up to
/// `max_body_bytes` each), so every pass is STREAMING — line by line through a
/// [`std::io::BufRead`], memory bounded by the longest line — never
/// `read_to_string` of the whole file (which pinned ~2× the file size in RAM
/// and blocked startup for its whole duration). A read-only pre-pass decides
/// whether anything would be dropped at all, so the common "everything still
/// in window" restart costs ONE sequential read and ZERO writes (a naive
/// streaming rewrite would copy the whole multi-GB file to a temp only to
/// discard it). Since the caller now runs this in the background AFTER the
/// listener is ready, live requests may [`append`] while the rewrite runs: the
/// commit step takes [`IO_LOCK`], copies any bytes appended past the scanned
/// offset verbatim onto the pruned temp file, and only then renames — so a
/// record appended mid-prune is never lost ("history behind live": kept-old
/// records keep their order, the live tail follows).
///
/// Strictly best-effort: a `None` path, a missing/unreadable file, or any IO
/// error on read/write leaves the file as-is (the temp file is discarded).
/// Never panics. The rewrite goes through a sibling temp file + atomic rename
/// so a crash mid-prune can't truncate the log.
pub fn prune(
    path: Option<&std::path::Path>,
    retention_days: u64,
    max_total_bytes: u64,
    now_ms: u64,
) {
    prune_lifetime(
        path,
        retention_days,
        max_total_bytes,
        now_ms,
        &|line, cutoff| {
            matches!(serde_json::from_str::<RawIoRecord>(line),
                Ok(record) if record.v == RECORD_VERSION && record.ts_ms >= cutoff)
        },
        &IO_LOCK,
    );
}

/// The generic lifetime-prune driver behind [`prune`] — shared with the
/// activity log ([`crate::tui::activity::prune`]), which has the identical
/// contract (append-only JSONL, appends in capture-time order, its own append
/// lock) but a different line schema. `keep(line, cutoff)` decides whether a
/// trimmed, non-empty line survives an age pass; `lock` must be the SAME lock
/// the file's appender takes, so the commit's tail copy + rename can't lose a
/// concurrent append.
pub(crate) fn prune_lifetime(
    path: Option<&std::path::Path>,
    retention_days: u64,
    max_total_bytes: u64,
    now_ms: u64,
    keep: &dyn Fn(&str, u64) -> bool,
    lock: &'static Mutex<()>,
) {
    if retention_days == 0 && max_total_bytes == 0 {
        return; // keep forever, any size
    }
    let Some(path) = path else {
        return;
    };
    // Age bound: 0 = no age limit (cutoff 0 keeps every real timestamp).
    let cutoff = if retention_days == 0 {
        0
    } else {
        now_ms.saturating_sub(retention_days.saturating_mul(MS_PER_DAY))
    };
    // Size bound: every whole line ending at or before `drop_before` is
    // dropped. Snapshotting the length here (not under the append lock) is
    // fine — appends only grow the file, so the computed offset only ever
    // UNDER-drops relative to the final length, never drops a fresh record.
    let drop_before = match std::fs::metadata(path).map(|m| m.len()) {
        Ok(len) if max_total_bytes > 0 && len > max_total_bytes => len - max_total_bytes,
        Ok(_) => 0,
        Err(_) => return, // missing/unreadable — nothing to prune
    };
    if drop_before == 0 {
        if retention_days == 0 {
            return; // size within cap, no age bound — nothing to do
        }
        if !needs_prune(path, cutoff, keep) {
            return; // nothing to drop — no temp written, original untouched
        }
    }
    let Some(stage) = prune_scan(path, cutoff, drop_before, keep) else {
        return; // nothing to drop, or scan failed — original left as-is
    };
    prune_commit(path, stage, lock);
}

/// Read-only pre-pass: would the retention window drop anything? `true` on the
/// first out-of-window, wrong-version, unparseable, or blank line — exactly
/// the lines [`prune_scan`] rewrites the file to shed. A missing/unreadable
/// file or a torn final line (no trailing newline — crash artifact or an
/// append racing this scan; the rewrite carries it over verbatim either way)
/// is not by itself a reason to rewrite.
fn needs_prune(path: &std::path::Path, cutoff: u64, keep: &dyn Fn(&str, u64) -> bool) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return false, // EOF, everything in window
            Ok(_) => {
                if !line.ends_with('\n') {
                    return false; // torn final line — not a drop candidate
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return true;
                }
                if !keep(trimmed, cutoff) {
                    return true;
                }
            }
            Err(_) => return false, // unreadable — leave the file as-is
        }
    }
}

/// A staged prune rewrite: the pruned temp file plus how many bytes of the
/// original were scanned to produce it (the tail past `consumed` is whatever
/// concurrent appends added while scanning — [`prune_commit`] carries it over).
struct PruneStage {
    tmp: std::path::PathBuf,
    consumed: u64,
}

/// Phase 1 (unlocked, streaming): scan `path` line by line, writing in-window
/// parseable records to a sibling temp file. Whole lines ending at or before
/// `drop_before` (the size-cap prefix) are dropped regardless of age; a line
/// straddling the boundary is kept (under-drop — the next pass converges).
/// Returns `None` when there is nothing to rewrite (every line kept verbatim,
/// or any IO error — the temp is discarded either way).
fn prune_scan(
    path: &std::path::Path,
    cutoff: u64,
    drop_before: u64,
    keep: &dyn Fn(&str, u64) -> bool,
) -> Option<PruneStage> {
    let file = std::fs::File::open(path).ok()?; // missing/unreadable = nothing to prune
    let mut reader = std::io::BufReader::new(file);
    let tmp = path.with_extension("jsonl.prune.tmp");
    // `create` truncates a stale temp left by a crashed prune.
    let out = std::fs::File::create(&tmp).ok()?;
    let mut writer = std::io::BufWriter::new(out);

    let mut consumed: u64 = 0;
    let mut changed = false;
    let mut line = String::new();
    let discard = |writer: std::io::BufWriter<std::fs::File>, tmp: &std::path::Path| {
        drop(writer);
        let _ = std::fs::remove_file(tmp);
    };
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(n) => {
                if !line.ends_with('\n') {
                    // Partial final line: either a crash artifact or an append
                    // racing this scan mid-write. Leave it for the commit's
                    // verbatim tail copy (do NOT count it as consumed) so it is
                    // carried over whole once the writer finishes.
                    break;
                }
                consumed += n as u64;
                if consumed <= drop_before {
                    // Inside the size-cap prefix: shed the whole line, oldest
                    // first, without parsing it.
                    changed = true;
                    continue;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    changed = true;
                    continue;
                }
                if keep(trimmed, cutoff) {
                    if writer
                        .write_all(trimmed.as_bytes())
                        .and_then(|()| writer.write_all(b"\n"))
                        .is_err()
                    {
                        discard(writer, &tmp);
                        return None;
                    }
                } else {
                    // Out of window, wrong version, or unparseable → drop it.
                    changed = true;
                }
            }
            Err(_) => {
                discard(writer, &tmp);
                return None;
            }
        }
    }
    // Nothing to do (every line kept verbatim), or the kept set failed to
    // flush: discard the temp and leave the original untouched.
    if !changed || writer.flush().is_err() {
        discard(writer, &tmp);
        return None;
    }
    Some(PruneStage { tmp, consumed })
}

/// Phase 2 (under [`IO_LOCK`]): copy any bytes appended past the scanned
/// offset verbatim onto the temp file, then atomically rename it over the
/// original. On any IO error the temp is discarded and the original is left
/// as-is.
fn prune_commit(path: &std::path::Path, stage: PruneStage, lock: &'static Mutex<()>) {
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let copied_tail = (|| -> std::io::Result<()> {
        let mut src = std::fs::File::open(path)?;
        let len = src.metadata()?.len();
        if len > stage.consumed {
            src.seek(std::io::SeekFrom::Start(stage.consumed))?;
            let mut out = std::fs::OpenOptions::new().append(true).open(&stage.tmp)?;
            std::io::copy(&mut src, &mut out)?;
        }
        Ok(())
    })();
    if copied_tail.is_err() || std::fs::rename(&stage.tmp, path).is_err() {
        let _ = std::fs::remove_file(&stage.tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: u64, ts_ms: u64) -> RawIoRecord {
        RawIoRecord::new(
            id,
            ts_ms,
            Some("claude".into()),
            Some("claude-sonnet-4".into()),
            Some("acct-a".into()),
            Some(200),
            br#"{"model":"m","messages":[]}"#,
            br#"{"id":"msg_1"}"#,
            RESPONSE_CAP_BYTES,
            Some(vec![("content-type".into(), "application/json".into())]),
            Some(vec![("request-id".into(), "req_1".into())]),
            None,
        )
    }

    fn tmp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "llmux-rawio-test-{}-{}-{tag}.jsonl",
            std::process::id(),
            ulid::Ulid::new()
        ))
    }

    #[test]
    fn record_round_trips_through_json_with_fields_intact() {
        let path = tmp_path("roundtrip");
        let record = rec(7, 1_700_000_000_000);
        append(Some(&path), &record);

        let contents = std::fs::read_to_string(&path).expect("file written");
        let parsed: RawIoRecord =
            serde_json::from_str(contents.trim()).expect("one parseable line");
        assert_eq!(parsed, record, "all fields survive the round-trip");
        assert_eq!(parsed.v, RECORD_VERSION);
        assert_eq!(parsed.id, 7);
        assert_eq!(parsed.status, Some(200));
        assert_eq!(parsed.request_body, r#"{"model":"m","messages":[]}"#);
        assert_eq!(parsed.response_body, r#"{"id":"msg_1"}"#);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn upstream_half_round_trips_and_stays_off_passthrough_records(/* UI-8 */) {
        let path = tmp_path("upstream");
        // A translated exchange carries the rewritten upstream half…
        let mut record = rec(9, 1_700_000_000_000);
        record.upstream = Some(UpstreamRaw {
            url: Some("https://api.example.com/responses".into()),
            request_body: Some(r#"{"input":[]}"#.into()),
            request_headers: Some(vec![("authorization".into(), "•••redacted".into())]),
            response_body: Some("event: response.completed\n\n".into()),
            response_headers: Some(vec![("x-request-id".into(), "req_9".into())]),
        });
        append(Some(&path), &record);
        let line = std::fs::read_to_string(&path).expect("written");
        let parsed: RawIoRecord = serde_json::from_str(line.trim()).expect("parseable");
        assert_eq!(parsed, record, "upstream half survives the round-trip");
        // …while a passthrough record (upstream: None) serializes WITHOUT the
        // key at all (additive schema), and a pre-UI-8 line parses to None.
        let _ = std::fs::remove_file(&path);
        append(Some(&path), &rec(1, 1));
        let line = std::fs::read_to_string(&path).expect("written");
        assert!(!line.contains("\"upstream\""), "absent, not null: {line}");
        let old: RawIoRecord = serde_json::from_str(line.trim()).expect("parseable");
        assert_eq!(old.upstream, None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn body_over_the_cap_is_truncated_with_a_marker() {
        let big = "a".repeat(RESPONSE_CAP_BYTES + 500);
        let record = RawIoRecord::new(
            1,
            0,
            None,
            None,
            None,
            None,
            big.as_bytes(),
            b"resp",
            RESPONSE_CAP_BYTES,
            None,
            None,
            None,
        );
        assert!(
            record.request_body.contains("…[truncated 500 bytes]"),
            "marks the exact dropped byte count"
        );
        assert!(
            record.request_body.len() <= RESPONSE_CAP_BYTES + 64,
            "clipped near the cap (+ short marker), got {}",
            record.request_body.len()
        );
        // A body at/under the cap is stored whole.
        assert_eq!(record.response_body, "resp", "small body kept whole");
    }

    #[test]
    fn body_at_exactly_the_cap_is_kept_whole() {
        let exact = "b".repeat(RESPONSE_CAP_BYTES);
        let record = RawIoRecord::new(
            1,
            0,
            None,
            None,
            None,
            None,
            exact.as_bytes(),
            b"",
            RESPONSE_CAP_BYTES,
            None,
            None,
            None,
        );
        assert_eq!(record.request_body.len(), RESPONSE_CAP_BYTES);
        assert!(!record.request_body.contains("truncated"));
    }

    #[test]
    fn truncation_respects_utf8_char_boundaries() {
        // A multi-byte char straddling the cap boundary must not be split.
        let prefix = "x".repeat(RESPONSE_CAP_BYTES - 1);
        let body = format!("{prefix}€€€"); // '€' is 3 bytes
        let record = RawIoRecord::new(
            1,
            0,
            None,
            None,
            None,
            None,
            body.as_bytes(),
            b"",
            RESPONSE_CAP_BYTES,
            None,
            None,
            None,
        );
        // The stored prefix (before the marker) must be valid UTF-8 by
        // construction (String), and must not include a partial '€'.
        assert!(record.request_body.contains("…[truncated"));
        let kept = record
            .request_body
            .split("…[truncated")
            .next()
            .expect("prefix");
        assert!(kept.is_char_boundary(kept.len()));
        let _ = String::from(kept); // valid UTF-8, no panic
    }

    #[test]
    fn max_body_bytes_override_is_respected() {
        // A body comfortably UNDER the 8 MiB default but OVER a small override
        // must be truncated at the override, proving the cap is configurable and
        // applies to BOTH request and response bodies.
        let body = "z".repeat(100);
        let record = RawIoRecord::new(
            1,
            0,
            None,
            None,
            None,
            None,
            body.as_bytes(),
            body.as_bytes(),
            32,
            None,
            None,
            None,
        );
        assert!(
            record.request_body.contains("…[truncated 68 bytes]"),
            "request body clipped at the override cap (32), got: {}",
            record.request_body
        );
        assert!(
            record.response_body.contains("…[truncated 68 bytes]"),
            "same override cap applies to the response body, got: {}",
            record.response_body
        );
        // And a body under the override is kept whole.
        let small = RawIoRecord::new(
            1, 0, None, None, None, None, b"hi", b"hi", 32, None, None, None,
        );
        assert_eq!(small.request_body, "hi");
        assert_eq!(small.response_body, "hi");
    }

    #[test]
    fn streamed_body_under_cap_is_kept_whole_no_marker() {
        // total == kept.len() ⇒ nothing dropped ⇒ no marker, body verbatim.
        let kept = b"event: message_start\n\n";
        let record = RawIoRecord::new_streamed(
            1,
            0,
            None,
            None,
            None,
            Some(200),
            b"{}",
            kept,
            kept.len(),
            RESPONSE_CAP_BYTES,
            None,
            None,
            None,
        );
        assert_eq!(record.response_body, String::from_utf8_lossy(kept));
        assert!(!record.response_body.contains("truncated"));
    }

    #[test]
    fn streamed_body_over_cap_marks_dropped_count_from_total() {
        // The relay retained only 10 bytes of a 1000-byte stream → the marker
        // must report the 990 bytes it dropped, which the bounded prefix alone
        // cannot reveal. This is the streamed truncation path.
        let kept = b"0123456789"; // 10 bytes retained
        let total = 1000usize; // 1000 bytes actually streamed
        let record = RawIoRecord::new_streamed(
            1,
            0,
            None,
            None,
            None,
            Some(200),
            b"{}",
            kept,
            total,
            10, // cap (matches what the relay retained)
            None,
            None,
            None,
        );
        assert_eq!(
            record.response_body, "0123456789…[truncated 990 bytes]",
            "kept prefix + accurate dropped count from the relay total"
        );
    }

    #[test]
    fn prune_keeps_recent_drops_old() {
        let path = tmp_path("prune");
        let now = 100 * MS_PER_DAY; // day 100
                                    // Old: day 1; recent: day 99. Retention 90 days → cutoff = day 10.
        append(Some(&path), &rec(1, MS_PER_DAY));
        append(Some(&path), &rec(2, 99 * MS_PER_DAY));

        prune(Some(&path), 90, 0, now);

        let contents = std::fs::read_to_string(&path).expect("file kept");
        let ids: Vec<u64> = contents
            .lines()
            .filter_map(|l| serde_json::from_str::<RawIoRecord>(l).ok())
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, vec![2], "only the in-window record survives");
        let _ = std::fs::remove_file(&path);
    }

    /// Issue #127: the size cap drops the OLDEST records (front of the file)
    /// until the kept set fits, independent of age.
    #[test]
    fn prune_size_cap_drops_oldest_until_under_cap() {
        let path = tmp_path("size-cap");
        for id in 0..40 {
            append(Some(&path), &rec(id, 1_000 + id));
        }
        let len = std::fs::metadata(&path).expect("meta").len();
        let cap = len / 2;
        // No age bound (retention 0) — only the size cap acts.
        prune(Some(&path), 0, cap, u64::MAX);

        let contents = std::fs::read_to_string(&path).expect("file kept");
        let ids: Vec<u64> = contents
            .lines()
            .filter_map(|l| serde_json::from_str::<RawIoRecord>(l).ok())
            .map(|r| r.id)
            .collect();
        assert!(!ids.is_empty(), "newest records survive");
        assert_eq!(*ids.last().expect("last"), 39, "newest record kept");
        assert!(ids[0] > 0, "oldest records dropped");
        let expected: Vec<u64> = (ids[0]..=39).collect();
        assert_eq!(ids, expected, "kept set is a contiguous newest suffix");
        // One straddling line of slack: the boundary line is kept, not torn.
        let line_len = len / 40;
        let after = std::fs::metadata(&path).expect("meta").len();
        assert!(
            after <= cap + line_len + 1,
            "post-prune size {after} within cap {cap} + one line {line_len}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Issue #127 / trinity P1 proof: after a size-cap rotation, the raw
    /// viewer's [`find_record`] still resolves records in the KEPT window and
    /// no longer finds the rotated-out oldest ones.
    #[test]
    fn find_record_after_size_prune_keeps_newest_window_only() {
        let path = tmp_path("size-cap-find");
        let base = 1_000_000;
        for id in 0..40 {
            append(Some(&path), &rec(id, base + id));
        }
        let len = std::fs::metadata(&path).expect("meta").len();
        prune(Some(&path), 0, len / 2, u64::MAX);

        let newest = find_record(Some(&path), 39, base + 39);
        assert!(
            newest.is_some_and(|r| r.id == 39),
            "newest record still resolvable after rotation"
        );
        let oldest = find_record(Some(&path), 0, base);
        assert!(oldest.is_none(), "rotated-out record is gone");
        let _ = std::fs::remove_file(&path);
    }

    /// Size cap 0 = uncapped: a file over any hypothetical size is untouched
    /// when retention is also 0.
    #[test]
    fn prune_size_cap_zero_keeps_everything() {
        let path = tmp_path("size-cap-zero");
        for id in 0..10 {
            append(Some(&path), &rec(id, 1_000 + id));
        }
        let before = std::fs::metadata(&path).expect("meta").len();
        prune(Some(&path), 0, 0, u64::MAX);
        let after = std::fs::metadata(&path).expect("meta").len();
        assert_eq!(before, after, "0/0 bounds never rewrite");
        let _ = std::fs::remove_file(&path);
    }

    /// Both bounds together: age drops the out-of-window head, size keeps the
    /// total under the cap — the stricter of the two wins.
    #[test]
    fn prune_age_and_size_bounds_compose() {
        let path = tmp_path("age-and-size");
        let now = 100 * MS_PER_DAY;
        // 2 ancient records (day 1) + 20 recent (day 99).
        append(Some(&path), &rec(0, MS_PER_DAY));
        append(Some(&path), &rec(1, MS_PER_DAY));
        for id in 2..22 {
            append(Some(&path), &rec(id, 99 * MS_PER_DAY));
        }
        let len = std::fs::metadata(&path).expect("meta").len();
        // Cap that also forces dropping some RECENT records beyond the aged ones.
        prune(Some(&path), 90, len / 4, now);
        let contents = std::fs::read_to_string(&path).expect("file kept");
        let ids: Vec<u64> = contents
            .lines()
            .filter_map(|l| serde_json::from_str::<RawIoRecord>(l).ok())
            .map(|r| r.id)
            .collect();
        assert!(!ids.contains(&0) && !ids.contains(&1), "aged records gone");
        assert_eq!(*ids.last().expect("last"), 21, "newest kept");
        assert!(ids.len() < 20, "size cap dropped recent records too");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn prune_with_zero_retention_keeps_all() {
        let path = tmp_path("prune-zero");
        append(Some(&path), &rec(1, 1)); // ancient
        append(Some(&path), &rec(2, 2));
        let before = std::fs::read_to_string(&path).expect("file");

        prune(Some(&path), 0, 0, u64::MAX);

        let after = std::fs::read_to_string(&path).expect("file");
        assert_eq!(before, after, "retention_days == 0 is a no-op");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn prune_tolerates_corrupt_lines_and_keeps_valid_recent_ones() {
        let path = tmp_path("prune-corrupt");
        let now = 100 * MS_PER_DAY;
        // A recent valid record, a corrupt line, and an old valid record.
        append(Some(&path), &rec(2, 99 * MS_PER_DAY));
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open");
            writeln!(f, "not json at all {{").expect("write");
        }
        append(Some(&path), &rec(1, MS_PER_DAY));

        prune(Some(&path), 90, 0, now);

        let contents = std::fs::read_to_string(&path).expect("file");
        let ids: Vec<u64> = contents
            .lines()
            .filter_map(|l| serde_json::from_str::<RawIoRecord>(l).ok())
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, vec![2], "corrupt + old dropped, recent kept");
        assert!(
            !contents.contains("not json"),
            "corrupt line shed on rewrite"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn prune_missing_file_is_a_noop() {
        let path = tmp_path("prune-missing");
        // Never created.
        prune(Some(&path), 90, 0, now_ms());
        assert!(!path.exists(), "prune does not create the file");
    }

    #[test]
    fn append_with_none_path_writes_nothing_and_never_panics() {
        // No path = disabled / no state dir → silent no-op.
        append(None, &rec(1, 1));
    }

    #[test]
    fn prune_with_none_path_is_a_noop() {
        prune(None, 90, 0, now_ms());
    }

    #[test]
    fn find_record_matches_id_within_the_ts_window() {
        let path = tmp_path("find");
        // Two daemon runs reuse id 5: an old run at day 1 and a fresh run at
        // day 2. Another id sits between them.
        let day = MS_PER_DAY;
        append(Some(&path), &rec(5, day));
        append(Some(&path), &rec(9, day + 500_000));
        append(Some(&path), &rec(5, 2 * day));

        // Looked up near its own completion time, each run's record wins.
        let new = find_record(Some(&path), 5, 2 * day + 1_000).expect("newest id-5");
        assert_eq!(new.ts_ms, 2 * day);
        let old = find_record(Some(&path), 5, day + 1_000).expect("oldest id-5");
        assert_eq!(old.ts_ms, day);
        // Headers written at capture time come back intact.
        assert_eq!(
            new.request_headers.as_deref(),
            Some(&[("content-type".to_string(), "application/json".to_string())][..])
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn find_record_prefers_the_closest_ts_within_one_window(/* trinity R2 */) {
        // Two daemon runs 2 minutes apart reuse id=1 — BOTH records sit inside
        // each other's ±5 min window, so the window alone cannot disambiguate.
        // The lookup must return the record CLOSEST to the entry's completion
        // time, not the first (= newest) match the backwards scan meets.
        let path = tmp_path("find-closest");
        let t_old = 100 * MS_PER_DAY; // "12:00" — pre-restart run
        let t_new = t_old + 120_000; // "12:02" — post-restart run, same id
        append(Some(&path), &rec(1, t_old));
        append(Some(&path), &rec(1, t_new));

        let old = find_record(Some(&path), 1, t_old + 500).expect("old entry resolves");
        assert_eq!(old.ts_ms, t_old, "12:00 activity gets the 12:00 raw record");
        let new = find_record(Some(&path), 1, t_new + 500).expect("new entry resolves");
        assert_eq!(new.ts_ms, t_new, "12:02 activity gets the 12:02 raw record");
        // Equidistant-ish query still lands on the strictly closer one.
        let mid = find_record(Some(&path), 1, t_old + 50_000).expect("mid query");
        assert_eq!(mid.ts_ms, t_old, "50s vs 70s away — closer record wins");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn find_record_misses_unknown_id_or_out_of_window() {
        let path = tmp_path("find-miss");
        append(Some(&path), &rec(5, MS_PER_DAY));
        // Unknown id.
        assert!(find_record(Some(&path), 6, MS_PER_DAY).is_none());
        // Same id, completion time far outside the ±5 min window.
        assert!(find_record(Some(&path), 5, 3 * MS_PER_DAY).is_none());
        // Disabled capture (no path) is a silent miss.
        assert!(find_record(None, 5, MS_PER_DAY).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pre_header_records_still_parse_with_headers_absent() {
        // A line written before the header fields existed must deserialize
        // with `None` headers (additive schema), and find_record must return it.
        let path = tmp_path("pre-header");
        let line = format!(
            "{{\"v\":1,\"ts_ms\":{},\"id\":3,\"request_body\":\"{{}}\",\"response_body\":\"ok\"}}",
            MS_PER_DAY
        );
        std::fs::write(&path, format!("{line}\n")).expect("write");
        let record = find_record(Some(&path), 3, MS_PER_DAY).expect("old line found");
        assert_eq!(record.request_headers, None);
        assert_eq!(record.response_headers, None);
        let _ = std::fs::remove_file(&path);
    }

    /// The common restart (nothing out of window) must be read-only: no temp
    /// file written, the log byte-identical — the pre-pass decides without
    /// staging a rewrite. (A multi-GB log would otherwise be fully copied to a
    /// temp and discarded on every restart.)
    #[test]
    fn prune_with_nothing_to_drop_writes_nothing() {
        let path = tmp_path("prune-noop");
        let now = 100 * MS_PER_DAY;
        append(Some(&path), &rec(1, 98 * MS_PER_DAY));
        append(Some(&path), &rec(2, 99 * MS_PER_DAY));
        let before = std::fs::read_to_string(&path).expect("file");

        prune(Some(&path), 90, 0, now);

        let after = std::fs::read_to_string(&path).expect("file");
        assert_eq!(before, after, "log untouched");
        assert!(
            !path.with_extension("jsonl.prune.tmp").exists(),
            "no temp file staged for a no-op prune"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A record appended BETWEEN the streaming scan and the commit (prune now
    /// runs in the background next to live traffic) must survive the rewrite:
    /// the commit copies the appended tail verbatim before renaming.
    #[test]
    fn prune_preserves_records_appended_during_scan() {
        let path = tmp_path("prune-tail");
        let now = 100 * MS_PER_DAY;
        append(Some(&path), &rec(1, MS_PER_DAY)); // old → dropped
        append(Some(&path), &rec(2, 99 * MS_PER_DAY)); // recent → kept

        // Drive the two phases by hand to interleave an append deterministically.
        let cutoff = now.saturating_sub(90 * MS_PER_DAY);
        let keep = |line: &str, cutoff: u64| {
            matches!(serde_json::from_str::<RawIoRecord>(line),
                Ok(record) if record.v == RECORD_VERSION && record.ts_ms >= cutoff)
        };
        let stage = prune_scan(&path, cutoff, 0, &keep).expect("old record → rewrite staged");
        append(Some(&path), &rec(3, 99 * MS_PER_DAY)); // lands after the scan
        prune_commit(&path, stage, &IO_LOCK);

        let contents = std::fs::read_to_string(&path).expect("file");
        let ids: Vec<u64> = contents
            .lines()
            .filter_map(|l| serde_json::from_str::<RawIoRecord>(l).ok())
            .map(|r| r.id)
            .collect();
        assert_eq!(
            ids,
            vec![2, 3],
            "old dropped; kept record first, mid-prune append preserved behind it"
        );
        let _ = std::fs::remove_file(&path);
    }
}
