//! SSE passthrough: stream upstream `text/event-stream` bodies to the client
//! with backpressure and client-disconnect detection, while extracting usage
//! from `message_start` / `message_delta` events for stats (FR1).
//!
//! Porting pitfall: events fragment across chunks — buffer to event boundary
//! (a blank line: `\n\n`, or its CRLF / lone-CR forms) before parsing; never
//! assume one chunk == one event.
//!
//! Byte-identity contract: the bytes sent to the client are the exact chunks
//! received from upstream — the [`EventBuffer`] only *observes* a copy for
//! usage stats and never rewrites the stream, so parse failures cannot
//! corrupt the relay.

use std::time::Duration;

use bytes::Bytes;
use tokio_stream::StreamExt as _;

use crate::tui::{SubUsage, TokenCounts};

/// Token usage extracted from a message stream. `input_tokens` is the FRESH
/// (non-cached) prompt count on both providers; the cached components are kept
/// separately and optionally — `None` means the upstream did not report that
/// field (rendered "—"), distinct from `Some(0)` (an explicit zero).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    /// The extended-TTL subset of `cache_creation_input_tokens` (Anthropic
    /// 1h, OpenAI 12h writes; [`extended_cache_writes`]). `None` = no split
    /// reported. See [`TokenCounts::cache_creation_extended`].
    pub cache_creation_extended: Option<u64>,
    /// Anthropic `usage.server_tool_use.web_search_requests`
    /// ([`web_search_requests`]). `None` = not reported.
    pub web_search_requests: Option<u64>,
    /// Anthropic `usage.iterations` entries the four counters above do NOT
    /// include, each billed at its own model's rates
    /// ([`uncounted_iterations`]). Empty for every other upstream.
    pub sub_usage: Vec<SubUsage>,
    /// The model that produced the returned message when a FALLBACK model
    /// served the turn ([`fallback_served_model`]): the four counters above
    /// are that model's, so it is priced and booked under it. `None` on every
    /// turn the requested model served.
    pub served_model: Option<String>,
    /// The response is an Anthropic refusal Anthropic does not bill (#283):
    /// `stop_reason: "refusal"` before any output (no output tokens) with a
    /// `stop_details.category` outside [`BILLED_REFUSAL_CATEGORIES`]
    /// ([`unbilled_refusal`]). Its tokens are still recorded; the request is
    /// priced at 0 ([`crate::pricing::PriceClass::UNBILLED`]).
    pub unbilled_refusal: bool,
}

impl StreamUsage {
    /// Accumulate another observation (saturating). For the optional cache
    /// counters the result is present iff at least one side reported a value.
    /// `sub_usage` / `served_model` are not per-event observations (the
    /// [`UsageObserver`] derives them once, at the end) and are left alone.
    pub fn add(&mut self, other: StreamUsage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_input_tokens =
            add_opt(self.cache_read_input_tokens, other.cache_read_input_tokens);
        self.cache_creation_input_tokens = add_opt(
            self.cache_creation_input_tokens,
            other.cache_creation_input_tokens,
        );
        self.cache_creation_extended =
            add_opt(self.cache_creation_extended, other.cache_creation_extended);
        self.web_search_requests = add_opt(self.web_search_requests, other.web_search_requests);
    }
}

/// Refusal categories Anthropic bills even when the refusal came before any
/// output: "a refusal that arrives before any output is billed when its
/// `stop_details.category` is `"bio"`, `"frontier_llm"`, or
/// `"reasoning_extraction"`" ("How refusals are billed",
/// https://platform.claude.com/docs/en/build-with-claude/refusals-and-fallback,
/// read 2026-11-11). A refusal that arrived mid-stream (it produced output)
/// is billed whatever its category.
pub const BILLED_REFUSAL_CATEGORIES: &[&str] = &["bio", "frontier_llm", "reasoning_extraction"];

/// Whether a response is an Anthropic refusal Anthropic does not bill (#283):
/// "a refusal that arrives before any output is billed when its
/// `stop_details.category` is `"bio"`, `"frontier_llm"`, or
/// `"reasoning_extraction"`. ... A refusal before any output in any other
/// category, or with a `null` category, is not billed" ("How refusals are
/// billed", build-with-claude/refusals-and-fallback, read 2026-11-11). Before
/// any output means no output tokens and, on a stream, no content delta
/// relayed; a refusal after output ("mid-stream") bills its input and the
/// output already streamed, like any request.
pub fn unbilled_refusal(
    stop_reason: Option<&str>,
    category: Option<&str>,
    output_tokens: u64,
    streamed_output: bool,
) -> bool {
    stop_reason == Some("refusal")
        && output_tokens == 0
        && !streamed_output
        && !category.is_some_and(|c| BILLED_REFUSAL_CATEGORIES.contains(&c))
}

/// One Anthropic `fallback` content block — the hand-off from a model that
/// declined to the one that continues:
/// `{"type":"fallback","from":{"model":..},"to":{"model":..},"trigger":{"type":"refusal","category":..}}`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FallbackHop {
    pub from_model: Option<String>,
    pub to_model: Option<String>,
    /// The declining model's refusal category (`None` = null / absent).
    pub category: Option<String>,
}

impl FallbackHop {
    /// Parse one content block; `None` unless it is a `fallback` block.
    pub fn from_block(block: &serde_json::Value) -> Option<Self> {
        if block.get("type").and_then(serde_json::Value::as_str) != Some("fallback") {
            return None;
        }
        let text = |pointer: &str| {
            block
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        Some(Self {
            from_model: text("/from/model"),
            to_model: text("/to/model"),
            category: text("/trigger/category"),
        })
    }
}

fn iteration_kind(it: &serde_json::Value) -> &str {
    it.get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

fn iteration_model(it: &serde_json::Value) -> Option<&str> {
    it.get("model").and_then(serde_json::Value::as_str)
}

/// The model that produced the returned message when a FALLBACK model served
/// the turn, else `None` (the requested model served it: attribution stays).
/// "The top-level `model` field reports the model that produced the returned
/// message" (build-with-claude/refusals-and-fallback, read 2026-11-11). A
/// fallback served when `usage.iterations` has a `fallback_message` entry or
/// a `fallback` block was seen. The model is read, in order, from that entry,
/// the last block's `to.model`, then `reported_model` (the JSON body's
/// `model`, or the stream's `message_start` model — which names the
/// REQUESTED model when the decline came mid-output, hence last).
pub fn fallback_served_model(
    usage: &serde_json::Value,
    hops: &[FallbackHop],
    reported_model: Option<&str>,
) -> Option<String> {
    let served = usage
        .get("iterations")
        .and_then(serde_json::Value::as_array)
        .and_then(|its| {
            its.iter()
                .rev()
                .find(|it| iteration_kind(it) == "fallback_message")
        });
    if served.is_none() && hops.is_empty() {
        return None;
    }
    served
        .and_then(iteration_model)
        .or_else(|| hops.last().and_then(|hop| hop.to_model.as_deref()))
        .or(reported_model)
        .map(str::to_string)
}

/// The Anthropic `usage.iterations` entries the top-level `usage` counters do
/// NOT include and Anthropic bills, as [`SubUsage`]s. `None` when `usage`
/// carries no `iterations` array (every non-Anthropic upstream, and requests
/// without an iterating feature).
///
/// Which entries the top level covers (all docs read 2026-11-11):
/// - Normally the `message` entries — the executor. `advisor_message`
///   ("Advisor tokens are not rolled into the top-level totals",
///   agents-and-tools/tool-use/advisor-tool), `compaction` ("The top-level
///   `input_tokens` and `output_tokens` do not include compaction iteration
///   usage", build-with-claude/compaction-threshold) and any type not known
///   today are extra, billed at their own model's rates.
/// - A response a fallback model served is the exception: "The top-level
///   `usage` counts describe only the attempt that produced the returned
///   message" — the `fallback_message` entry — while each declined attempt
///   is a `message` entry at its own model's rates
///   (build-with-claude/refusals-and-fallback). A declined attempt is billed
///   only when it streamed output, or when its refusal category (from the
///   `fallback` block in `hops` that names it) is in
///   [`BILLED_REFUSAL_CATEGORIES`]. A block is paired to an attempt by
///   model; a single unpaired attempt and a single unpaired block are paired
///   with each other. An attempt left without a category is booked only if it
///   produced output. A `message` entry naming the served model is that
///   model's own work and stays covered.
///
/// An entry with no `type` or with all-zero counters is skipped. The entry's
/// `model` is kept as reported; `None` means the request's own model.
pub fn uncounted_iterations(
    usage: &serde_json::Value,
    hops: &[FallbackHop],
) -> Option<Vec<SubUsage>> {
    let iterations = usage.get("iterations")?.as_array()?;
    let served: Vec<Option<&str>> = iterations
        .iter()
        .filter(|it| iteration_kind(it) == "fallback_message")
        .map(iteration_model)
        .collect();
    let declined: Vec<usize> = iterations
        .iter()
        .enumerate()
        .filter(|(_, it)| {
            !served.is_empty()
                && iteration_kind(it) == "message"
                && !served.contains(&iteration_model(it))
        })
        .map(|(index, _)| index)
        .collect();
    // Pair each declined attempt with the `fallback` block that names it.
    let same_model = |a: &str, b: &str| {
        crate::tui::activity::normalize_model(a)
            .eq_ignore_ascii_case(&crate::tui::activity::normalize_model(b))
    };
    let mut category: std::collections::HashMap<usize, Option<&str>> =
        std::collections::HashMap::new();
    let mut used = vec![false; hops.len()];
    for &index in &declined {
        let Some(model) = iteration_model(&iterations[index]) else {
            continue;
        };
        if let Some(hop) = (0..hops.len()).find(|&h| {
            !used[h]
                && hops[h]
                    .from_model
                    .as_deref()
                    .is_some_and(|from| same_model(from, model))
        }) {
            used[hop] = true;
            category.insert(index, hops[hop].category.as_deref());
        }
    }
    let unpaired: Vec<usize> = declined
        .iter()
        .copied()
        .filter(|index| !category.contains_key(index))
        .collect();
    let unused: Vec<usize> = (0..hops.len()).filter(|&h| !used[h]).collect();
    if let ([index], [hop]) = (unpaired.as_slice(), unused.as_slice()) {
        category.insert(*index, hops[*hop].category.as_deref());
    }
    let billed = |index: usize| {
        let output = iterations[index]
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        output > 0
            || category
                .get(&index)
                .copied()
                .flatten()
                .is_some_and(|c| BILLED_REFUSAL_CATEGORIES.contains(&c))
    };
    let uncounted = iterations
        .iter()
        .enumerate()
        .filter(|(index, it)| match iteration_kind(it) {
            "" | "fallback_message" => false,
            "message" => declined.contains(index) && billed(*index),
            _ => true,
        })
        .map(|(_, it)| it)
        .map(|it| {
            let count = |key: &str| it.get(key).and_then(serde_json::Value::as_u64);
            SubUsage {
                model: iteration_model(it).map(str::to_string),
                tokens: TokenCounts {
                    input: count("input_tokens").unwrap_or(0),
                    output: count("output_tokens").unwrap_or(0),
                    cache_read: count("cache_read_input_tokens"),
                    cache_creation: count("cache_creation_input_tokens"),
                    // Each entry carries its own `cache_creation` split
                    // (prompt-caching). Server-tool counts are request-level
                    // (`usage.server_tool_use`), so never read per entry.
                    cache_creation_extended: extended_cache_writes(
                        it,
                        count("cache_creation_input_tokens"),
                    ),
                    web_search_requests: None,
                },
                price_class: crate::pricing::PriceClass::STANDARD,
            }
        })
        .filter(|sub| sub.tokens.total() > 0)
        .collect();
    Some(uncounted)
}

/// The extended-TTL subset of one usage object's cache writes (#278): the
/// Anthropic 1-hour writes (`cache_creation.ephemeral_1h_input_tokens`; "The
/// `cache_creation_input_tokens` field equals the sum of the values in the
/// `cache_creation` object", build-with-claude/prompt-caching) or the OpenAI
/// 12-hour writes (`input_tokens_details.cache_write_tokens_details.
/// ttl_12h_tokens`, which with `ttl_30m_tokens` sums to `cache_write_tokens`:
/// developers.openai.com/api/docs/guides/prompt-caching and the Responses API
/// reference), both read 2026-11-11. `total` is the request's cache-write
/// count; a split larger than it is malformed and clamped to it, and without
/// a total (never seen on the wire) there is nothing to be a subset of, so
/// `None`. `None` too when the object carries no split — those writes are
/// priced at the short-TTL rate, as before the split was recorded.
pub fn extended_cache_writes(usage: &serde_json::Value, total: Option<u64>) -> Option<u64> {
    let split = extended_split(usage)?;
    total.map(|total| split.min(total))
}

/// The raw extended-TTL count of [`extended_cache_writes`], before the clamp.
fn extended_split(usage: &serde_json::Value) -> Option<u64> {
    usage
        .pointer("/cache_creation/ephemeral_1h_input_tokens")
        .or_else(|| {
            usage.pointer("/input_tokens_details/cache_write_tokens_details/ttl_12h_tokens")
        })
        .and_then(serde_json::Value::as_u64)
}

/// Anthropic server-side web searches one usage object reports
/// (`server_tool_use.web_search_requests`, platform.claude.com/docs/en/
/// about-claude/pricing "Web search tool", read 2026-11-11). Web fetches
/// (`web_fetch_requests`) are free on the same page and are not read.
pub fn web_search_requests(usage: &serde_json::Value) -> Option<u64> {
    usage
        .pointer("/server_tool_use/web_search_requests")
        .and_then(serde_json::Value::as_u64)
}

/// Saturating add of two optional counters where `None` means "unavailable":
/// the sum is present iff at least one operand reported a value. Shared by the
/// stream-usage accumulator and the model-usage aggregation.
pub fn add_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.saturating_add(y)),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

/// Reassembles SSE events from arbitrarily fragmented chunks. Push bytes in,
/// get complete events out; partial trailing data stays buffered.
///
/// Line endings are normalized on the way in: the SSE spec (WHATWG HTML
/// §9.2.6) ends a line with CRLF, LF or a lone CR, so `\r\n` and a lone `\r`
/// become `\n` before the `\n\n` event boundary is searched — every consumer
/// sees LF-only events whatever the upstream's framing. An LF-only stream
/// passes through byte-identical.
#[derive(Debug, Default)]
pub struct EventBuffer {
    buf: Vec<u8>,
    /// The last byte pushed was a `\r` not yet known to be a lone CR or the
    /// first half of a CRLF; the next byte (or the stream's end) decides.
    pending_cr: bool,
}

impl EventBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `chunk` to the buffer with CRLF / lone CR rewritten to LF.
    fn append_normalized(&mut self, chunk: &[u8]) {
        if !self.pending_cr && !chunk.contains(&b'\r') {
            // LF-only fast path: byte-identical to the pre-normalization
            // buffer.
            self.buf.extend_from_slice(chunk);
            return;
        }
        for &byte in chunk {
            if self.pending_cr {
                self.pending_cr = false;
                // The held `\r` is a line end either way; a following `\n`
                // completes it as CRLF and is consumed with it.
                self.buf.push(b'\n');
                if byte == b'\n' {
                    continue;
                }
            }
            if byte == b'\r' {
                self.pending_cr = true;
            } else {
                self.buf.push(byte);
            }
        }
    }

    /// Append a chunk and drain every COMPLETE event (terminated by a blank
    /// line — `\n\n` after line-ending normalization) accumulated so far, in
    /// order. Whitespace-only events (stray blank lines) are skipped.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.append_normalized(chunk);
        let mut events = Vec::new();
        while let Some(pos) = self.buf.windows(2).position(|w| w == b"\n\n") {
            let event: Vec<u8> = self.buf.drain(..pos + 2).collect();
            let text = String::from_utf8_lossy(&event[..pos]);
            if !text.trim().is_empty() {
                events.push(text.into_owned());
            }
        }
        events
    }

    /// Drain whatever is left after the stream ends — an unterminated final
    /// event still gets parsed for usage (mirrors the teamclaude tail parse).
    pub fn take_remainder(&mut self) -> Option<String> {
        // A `\r` held at the stream's end was a line end.
        if std::mem::take(&mut self.pending_cr) {
            self.buf.push(b'\n');
        }
        let text = String::from_utf8_lossy(&self.buf);
        let out = if text.trim().is_empty() {
            None
        } else {
            Some(text.into_owned())
        };
        self.buf.clear();
        out
    }
}

/// Extract usage from one complete SSE event, if it is a `message_start`
/// (input tokens) or `message_delta` (output tokens) event. Malformed
/// events yield `None` — usage stats are best-effort, never fatal.
pub fn extract_usage(event: &str) -> Option<StreamUsage> {
    let data = event.lines().find_map(|line| {
        line.strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
    })?;
    let value: serde_json::Value = serde_json::from_str(data.trim()).ok()?;
    match value.get("type")?.as_str()? {
        "message_start" => {
            let usage = value.get("message")?.get("usage")?;
            let input = usage.get("input_tokens")?.as_u64()?;
            // Anthropic prompt-caching counters, present only when the
            // request used caching — captured opportunistically (req8/9).
            let written = usage
                .get("cache_creation_input_tokens")
                .and_then(serde_json::Value::as_u64);
            Some(StreamUsage {
                input_tokens: input,
                output_tokens: 0,
                cache_read_input_tokens: usage
                    .get("cache_read_input_tokens")
                    .and_then(serde_json::Value::as_u64),
                cache_creation_input_tokens: written,
                cache_creation_extended: extended_cache_writes(usage, written),
                web_search_requests: web_search_requests(usage),
                sub_usage: Vec::new(),
                served_model: None,
                unbilled_refusal: false,
            })
        }
        "response.completed" | "response.incomplete" | "response.failed" => {
            // Native `run --codex` passthrough. OpenAI's `input_tokens` is
            // cache-inclusive: the cache reads (`cached_tokens`) and the cache
            // writes (`cache_write_tokens`) are disjoint parts of it
            // (developers.openai.com/api/docs/guides/prompt-caching, read
            // 2026-11-11), so fresh input is what is left after both and the
            // writes are cache creation. Same rule as the translate path
            // (`crate::provider::responses`).
            let usage = value.get("response")?.get("usage")?;
            let input = usage.get("input_tokens")?.as_u64()?;
            let cache = usage
                .pointer("/input_tokens_details/cached_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let written = usage
                .pointer("/input_tokens_details/cache_write_tokens")
                .and_then(serde_json::Value::as_u64);
            Some(StreamUsage {
                input_tokens: input
                    .saturating_sub(cache)
                    .saturating_sub(written.unwrap_or(0)),
                output_tokens: usage
                    .get("output_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                cache_read_input_tokens: Some(cache),
                cache_creation_input_tokens: written,
                cache_creation_extended: extended_cache_writes(usage, written),
                web_search_requests: None,
                sub_usage: Vec::new(),
                served_model: None,
                unbilled_refusal: false,
            })
        }
        "message_delta" => {
            let usage = value.get("usage")?;
            let output = usage.get("output_tokens")?.as_u64()?;
            Some(StreamUsage {
                input_tokens: 0,
                output_tokens: output,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                cache_creation_extended: None,
                web_search_requests: None,
                sub_usage: Vec::new(),
                served_model: None,
                unbilled_refusal: false,
            })
        }
        _ => None,
    }
}

/// Usage observer for one relayed Anthropic stream: folds every event, then
/// [`Self::finish`] yields the request's [`StreamUsage`].
///
/// Anthropic `message_delta` counters are cumulative when present (the SDK
/// bridge includes final input/cache totals after internal WebSearch rounds).
/// `usage.iterations` is cumulative too (the final `message_delta` carries
/// the updated array, advisor-tool "Streaming"), so the latest usage object
/// carrying one is kept; `fallback` content blocks and the `message_start`
/// model are kept to price a fallback-served turn.
#[derive(Debug, Default)]
pub struct UsageObserver {
    usage: StreamUsage,
    /// The latest `usage` object that carried `iterations`.
    iterations: Option<serde_json::Value>,
    hops: Vec<FallbackHop>,
    start_model: Option<String>,
    /// The stream's `stop_reason` and refusal category (`message_delta`
    /// `delta.stop_reason` / `delta.stop_details.category`), and whether the
    /// CURRENT attempt relayed a non-empty content delta (reset at each
    /// `fallback` block) — for [`unbilled_refusal`].
    stop_reason: Option<String>,
    refusal_category: Option<String>,
    streamed_output: bool,
}

impl UsageObserver {
    /// Fold one complete SSE event. Malformed events are ignored.
    pub fn observe(&mut self, event: &str) {
        let data = event
            .lines()
            .find_map(|line| line.strip_prefix("data:").map(str::trim));
        let value = data.and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok());
        let Some(v) = value else {
            return;
        };
        match v["type"].as_str() {
            Some("message_delta") => {
                if let Some(reason) = v
                    .pointer("/delta/stop_reason")
                    .and_then(serde_json::Value::as_str)
                {
                    self.stop_reason = Some(reason.to_string());
                    self.refusal_category = v
                        .pointer("/delta/stop_details/category")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);
                }
                let u = &v["usage"];
                if let Some(n) = u["input_tokens"].as_u64() {
                    self.usage.input_tokens = n;
                }
                if let Some(n) = u["output_tokens"].as_u64() {
                    self.usage.output_tokens = n;
                }
                if let Some(n) = u["cache_read_input_tokens"].as_u64() {
                    self.usage.cache_read_input_tokens = Some(n);
                }
                if let Some(n) = u["cache_creation_input_tokens"].as_u64() {
                    self.usage.cache_creation_input_tokens = Some(n);
                }
                // The TTL split and the web-search count are cumulative like
                // the counters; the split is clamped to the total at finish.
                if let Some(n) = extended_split(u) {
                    self.usage.cache_creation_extended = Some(n);
                }
                if let Some(n) = web_search_requests(u) {
                    self.usage.web_search_requests = Some(n);
                }
                if u.get("iterations").is_some() {
                    self.iterations = Some(u.clone());
                }
                return;
            }
            Some("message_start") => {
                self.start_model = v
                    .pointer("/message/model")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                if let Some(u) = v
                    .pointer("/message/usage")
                    .filter(|u| u.get("iterations").is_some())
                {
                    self.iterations = Some(u.clone());
                }
            }
            Some("content_block_start") => {
                if let Some(hop) = FallbackHop::from_block(&v["content_block"]) {
                    self.hops.push(hop);
                    // A fallback block starts the next attempt: the refusal
                    // rule applies to the attempt whose counters are priced
                    // (the last one), so only ITS output counts.
                    self.streamed_output = false;
                }
                return;
            }
            Some("content_block_delta") => {
                if !self.streamed_output && contains_content_delta(event.as_bytes()) {
                    self.streamed_output = true;
                }
                return;
            }
            _ => {}
        }
        if let Some(observed) = extract_usage(event) {
            self.usage.add(observed);
        }
    }

    /// The request's usage: the counters, the sub-usages the top level leaves
    /// out, and the served model of a fallback turn.
    ///
    /// On a fallback-served turn the four counters come from the
    /// `fallback_message` entry: "The top-level `usage` counts describe only
    /// the attempt that produced the returned message" (refusals-and-
    /// fallback, read 2026-11-11), while `message_start` may carry the
    /// DECLINING model's usage and a final `message_delta` need not repeat
    /// every counter. So does the extended-TTL write subset (#278).
    pub fn finish(self) -> StreamUsage {
        let mut usage = self.usage;
        // A later delta may report a total below an earlier split.
        usage.cache_creation_extended = usage
            .cache_creation_extended
            .zip(usage.cache_creation_input_tokens)
            .map(|(extended, total)| extended.min(total));
        let iterations = self.iterations.unwrap_or(serde_json::Value::Null);
        if let Some(served) = iterations
            .get("iterations")
            .and_then(serde_json::Value::as_array)
            .and_then(|its| {
                its.iter()
                    .rev()
                    .find(|it| iteration_kind(it) == "fallback_message")
            })
        {
            let count = |key: &str| served.get(key).and_then(serde_json::Value::as_u64);
            usage.input_tokens = count("input_tokens").unwrap_or(0);
            usage.output_tokens = count("output_tokens").unwrap_or(0);
            usage.cache_read_input_tokens = count("cache_read_input_tokens");
            usage.cache_creation_input_tokens = count("cache_creation_input_tokens");
            // Its TTL split too (#278): message_start's belongs to the
            // declining attempt. The web-search count is request-level
            // (`usage.server_tool_use`) and stays as observed.
            usage.cache_creation_extended =
                extended_cache_writes(served, usage.cache_creation_input_tokens);
        }
        usage.sub_usage = uncounted_iterations(&iterations, &self.hops).unwrap_or_default();
        usage.unbilled_refusal = unbilled_refusal(
            self.stop_reason.as_deref(),
            self.refusal_category.as_deref(),
            usage.output_tokens,
            self.streamed_output,
        );
        usage.served_model =
            fallback_served_model(&iterations, &self.hops, self.start_model.as_deref());
        usage
    }
}

/// Wall-clock landmarks of one relayed stream, captured inside the pump and
/// handed to `finish` for activity timing (perf telemetry v1):
/// - `first_byte` — the instant the FIRST successful upstream body chunk
///   arrived (TTFB).
/// - `first_content` — the instant the FIRST `content_block_delta` event was
///   observed on the downstream-normalized output ("first streamed output
///   delta"; text_delta / thinking_delta / input_json_delta alike —
///   deliberately NOT text-only, so thinking time stays inside the
///   post-delta window and the numerator/denominator agree).
///
/// `None` means the stream ended before that landmark. Instants (not
/// durations) so `finish` can compute offsets against the request's own
/// start.
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamTiming {
    pub first_byte: Option<std::time::Instant>,
    pub first_content: Option<std::time::Instant>,
    /// The instant the pump observed the upstream stream END (EOF or error)
    /// — captured INSIDE the pump, before any finish-side raw-io/trace/log
    /// work, so the post-delta span never absorbs post-processing time.
    pub stream_end: Option<std::time::Instant>,
    /// An SSE `error` event was observed on the stream — a protocol-level
    /// provider failure that arrives under a transport-clean HTTP 200.
    pub saw_error_event: bool,
    /// The CLIENT disconnected mid-relay. Not a provider failure — and the
    /// stream was truncated on OUR side, so no post-delta span may be
    /// claimed from it (see [`Self::gen_ms`]).
    pub client_gone: bool,
}

impl StreamTiming {
    /// Record a successful body-chunk arrival (first call wins).
    pub fn on_chunk(&mut self) {
        if self.first_byte.is_none() {
            self.first_byte = Some(std::time::Instant::now());
        }
    }

    /// Observe an SSE payload headed to the client; latches `first_content`
    /// on the first content delta and flags protocol-level `error` events.
    pub fn on_payload(&mut self, payload: &[u8]) {
        if self.first_content.is_none() && contains_content_delta(payload) {
            self.first_content = Some(std::time::Instant::now());
        }
        if !self.saw_error_event && contains_error_event(payload) {
            self.saw_error_event = true;
        }
    }

    /// Record that the upstream stream ended NOW (first call wins).
    pub fn on_stream_end(&mut self) {
        if self.stream_end.is_none() {
            self.stream_end = Some(std::time::Instant::now());
        }
    }

    /// The stream-side post-delta span in millis (first delta → stream end),
    /// when both landmarks were observed. `None` after a client disconnect —
    /// the relay stopped pulling, so the span would measure OUR truncation,
    /// not the provider's generation.
    pub fn gen_ms(&self) -> Option<u64> {
        if self.client_gone {
            return None;
        }
        let (fc, end) = (self.first_content?, self.stream_end?);
        Some(u64::try_from(end.saturating_duration_since(fc).as_millis()).unwrap_or(u64::MAX))
    }
}

/// Whether `payload` carries an SSE `error` event (event name line or JSON
/// `data.type == "error"`) — the protocol-level failure marker.
pub fn contains_error_event(payload: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(payload) else {
        return false;
    };
    for chunk in text.split("\n\n") {
        for line in chunk.lines() {
            if let Some(name) = line.strip_prefix("event:") {
                if matches!(
                    name.trim(),
                    "error" | "response.failed" | "response.incomplete"
                ) {
                    return true;
                }
            } else if let Some(d) = line
                .strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
            {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(d.trim()) {
                    if matches!(
                        value.get("type").and_then(serde_json::Value::as_str),
                        Some("error" | "response.failed" | "response.incomplete")
                    ) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Whether `payload` carries a NON-EMPTY Anthropic `content_block_delta`
/// event — the "first streamed output delta" latch for [`StreamTiming`].
/// Structured check (not a substring scan): each complete SSE event is
/// identified by its `event:` name line or its JSON `data.type`, and only a
/// delta whose `text` / `thinking` / `partial_json` value is non-empty
/// counts — framing, empty deltas, and lifecycle events that merely CONTAIN
/// the token (e.g. a tool named after it) can never latch. Called only until
/// the first latch, so the per-event JSON parse is bounded.
pub fn contains_content_delta(payload: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(payload) else {
        return false;
    };
    for chunk in text.split("\n\n") {
        for line in chunk.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data.trim()) {
                    if matches!(
                        v["type"].as_str(),
                        Some(
                            "response.output_text.delta"
                                | "response.function_call_arguments.delta"
                                | "response.custom_tool_call_input.delta"
                                | "response.reasoning_summary_text.delta"
                        )
                    ) && v["delta"].as_str().is_some_and(|s| !s.is_empty())
                    {
                        return true;
                    }
                }
            }
        }
        let mut named_delta = false;
        let mut data: Option<&str> = None;
        for line in chunk.lines() {
            if let Some(name) = line.strip_prefix("event:") {
                named_delta = name.trim() == "content_block_delta";
            } else if let Some(d) = line
                .strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
            {
                data = Some(d.trim());
            }
        }
        let Some(data) = data else { continue };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        let typed_delta =
            value.get("type").and_then(serde_json::Value::as_str) == Some("content_block_delta");
        if !(named_delta || typed_delta) {
            continue;
        }
        let non_empty = value.get("delta").is_some_and(|d| {
            ["text", "thinking", "partial_json"].iter().any(|k| {
                d.get(k)
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|v| !v.is_empty())
            })
        });
        if non_empty {
            return true;
        }
    }
    false
}

/// Stateful per-request SSE transformer: upstream events in, downstream SSE
/// bytes out. The codex provider's Responses→Anthropic converter implements
/// this; the Anthropic passthrough never goes through it (byte-identity path
/// untouched).
pub trait SseTransform: Send {
    /// One COMPLETE upstream event (terminated `\n\n` already stripped) →
    /// zero or more downstream SSE bytes.
    fn on_event(&mut self, event: &str) -> Vec<u8>;

    /// Upstream ended (cleanly or not) — flush any termination events.
    fn on_end(&mut self) -> Vec<u8>;

    /// Usage accumulated from the EMITTED Anthropic events, for the
    /// dashboard totals.
    fn usage(&self) -> StreamUsage;
}

/// Relay an upstream SSE response through a [`SseTransform`]: upstream
/// chunks are reassembled into complete events, each event is fed to the
/// transform, and the transform's OUTPUT bytes are what the client receives
/// (this is the codex path; the byte-identity path is
/// [`passthrough_body`]). Backpressure/disconnect semantics are identical to
/// the passthrough pump.
///
/// `finish` receives the transform's usage, THREE independent observe-only
/// buffers, the upstream error if one aborted the stream, the finished
/// transform, and whether the client disconnected:
/// - `captured` — the first `capture_limit` emitted bytes (short debug log
///   excerpt).
/// - `raw_captured` — the first `raw_capture_limit` emitted bytes (raw-io
///   full-payload tee).
/// - `upstream_captured` — the first `raw_capture_limit` UPSTREAM bytes,
///   verbatim as they arrived BEFORE transformation (the raw viewer's
///   api→proxy payload; 4-payload UI-8).
///
/// The first two are filled from the same emitted output, capped
/// independently; the third observes the inbound chunks. Each emitted chunk is
/// `tx.send`'d to the client FIRST; the copies are a side effect. Callers move
/// the account lease into `finish` (never switch mid-stream).
pub fn transform_body<T, F>(
    upstream: reqwest::Response,
    mut transform: T,
    capture_limit: usize,
    raw_capture_limit: usize,
    finish: F,
) -> axum::body::Body
where
    T: SseTransform + 'static,
    // `finish` also receives the finished transform (for converter-level detail
    // like the codex trace's raw usage / event count) and whether the relay
    // ended because the CLIENT disconnected (vs. upstream completing).
    F: FnOnce(StreamUsage, Vec<u8>, RawCapture, RawCapture, Option<String>, &T, bool, StreamTiming)
        + Send
        + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    tokio::spawn(async move {
        let mut events = EventBuffer::new();
        let mut captured: Vec<u8> = Vec::new();
        let mut raw_captured = RawCapture::new(raw_capture_limit);
        let mut upstream_captured = RawCapture::new(raw_capture_limit);
        let mut error: Option<String> = None;
        let mut timing = StreamTiming::default();
        let mut stream = Box::pin(upstream.bytes_stream());
        let mut client_gone = false;
        'pump: while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    timing.on_chunk();
                    upstream_captured.push(&chunk);
                    for event in events.push(&chunk) {
                        let out = transform.on_event(&event);
                        if out.is_empty() {
                            continue;
                        }
                        // Send FIRST, then observe both buffers.
                        // Observe timing BEFORE the client send: the latch
                        // must not absorb client backpressure.
                        timing.on_payload(&out);
                        if tx.send(Ok(Bytes::from(out.clone()))).await.is_err() {
                            client_gone = true;
                            timing.client_gone = true;
                            break 'pump;
                        }
                        capture(&mut captured, &out, capture_limit);
                        raw_captured.push(&out);
                    }
                }
                Err(err) => {
                    error = Some(err.to_string());
                    break;
                }
            }
        }
        timing.on_stream_end();
        if let Some(rest) = events.take_remainder() {
            let out = transform.on_event(&rest);
            if !out.is_empty() && !client_gone {
                timing.on_payload(&out);
                if tx.send(Ok(Bytes::from(out.clone()))).await.is_err() {
                    client_gone = true;
                    timing.client_gone = true;
                } else {
                    capture(&mut captured, &out, capture_limit);
                    raw_captured.push(&out);
                }
            }
        }
        let tail = transform.on_end();
        if !tail.is_empty() && !client_gone && tx.send(Ok(Bytes::from(tail.clone()))).await.is_ok()
        {
            capture(&mut captured, &tail, capture_limit);
            raw_captured.push(&tail);
        }
        if let Some(err) = &error {
            if !client_gone {
                let _ = tx.send(Err(std::io::Error::other(err.clone()))).await;
            }
        }
        finish(
            transform.usage(),
            captured,
            raw_captured,
            upstream_captured,
            error,
            &transform,
            client_gone,
            timing,
        );
    });
    axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
}

fn capture(captured: &mut Vec<u8>, chunk: &[u8], limit: usize) {
    if captured.len() < limit {
        let room = limit - captured.len();
        captured.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }
}

/// Observe-only accumulator for the raw-io full-payload tee: keeps at most
/// `limit` bytes (memory bound) while counting the TOTAL bytes seen, so a body
/// that overflows the cap can still be truncation-marked with the exact dropped
/// count — unlike a bare `Vec` cap, which loses the overflow size. Filled AFTER
/// each chunk is forwarded to the client; it never feeds back into the stream.
#[derive(Debug, Default)]
pub struct RawCapture {
    bytes: Vec<u8>,
    total: usize,
    limit: usize,
}

impl RawCapture {
    /// A tee bounded to `limit` retained bytes.
    pub fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            total: 0,
            limit,
        }
    }

    /// Observe a chunk already forwarded to the client: count all of it, retain
    /// up to the cap.
    pub fn push(&mut self, chunk: &[u8]) {
        self.total = self.total.saturating_add(chunk.len());
        capture(&mut self.bytes, chunk, self.limit);
    }

    /// Total bytes seen (including those dropped past the cap).
    pub fn total(&self) -> usize {
        self.total
    }

    /// The retained (bounded) bytes; the kept prefix when the body overflowed.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Relay an upstream SSE response as an axum body, observing usage on the
/// side. The pump task ends when upstream finishes OR the client disconnects
/// (the channel receiver is dropped, `send` fails, and we stop polling
/// upstream — dropping the `reqwest::Response` closes the upstream stream).
///
/// `finish` runs exactly once when the relay ends, with the accumulated usage,
/// TWO independent observe-only buffers, and the upstream error if one aborted
/// the stream:
/// - `captured` — the first `capture_limit` relayed bytes (the short debug
///   request-log excerpt, typically 8 KiB).
/// - `raw_captured` — the first `raw_capture_limit` relayed bytes (the raw-io
///   full-payload tee, typically `max_body_bytes` = 8 MiB).
///
/// Both are filled from the SAME forwarded chunks but capped independently, so
/// the debug log stays a short excerpt while raw-io retains the full (bounded)
/// body. Each chunk is `tx.send`'d to the client FIRST; the copies are a side
/// effect that never blocks, slows, or mutates the relayed bytes.
///
/// Callers move the account lease into this closure so the account stays pinned
/// for the stream's lifetime — errors after this point propagate to the client
/// as a broken body, never as an account switch (never switch mid-stream).
///
/// `idle_timeout` bounds upstream silence: if the next chunk does not arrive
/// within it, the relay aborts the stream the same way a transport error does
/// (records an error string, forwards an `Err(io::Error)` on the body channel,
/// then breaks) so the client sees a broken body rather than hanging forever.
/// `finish` still runs exactly once on that path, dropping the moved-in lease —
/// so the account is released, not pinned. This is an inactivity ceiling, reset
/// on every received chunk; long legitimate LLM streams with quiet gaps below
/// the ceiling pass through untouched. Defense in depth with the client's
/// `read_timeout` ([`crate::proxy::server::AppState::new`]).
pub fn passthrough_body<F>(
    upstream: reqwest::Response,
    capture_limit: usize,
    raw_capture_limit: usize,
    idle_timeout: Duration,
    finish: F,
) -> axum::body::Body
where
    F: FnOnce(StreamUsage, Vec<u8>, RawCapture, Option<String>, StreamTiming) + Send + 'static,
{
    let native_responses = upstream.headers().contains_key("x-llmux-native-responses");
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    tokio::spawn(async move {
        let mut events = EventBuffer::new();
        let mut usage = UsageObserver::default();
        let mut captured: Vec<u8> = Vec::new();
        let mut raw_captured = RawCapture::new(raw_capture_limit);
        let mut error: Option<String> = None;
        let mut timing = StreamTiming::default();
        let mut terminal = false;
        let mut stream = Box::pin(upstream.bytes_stream());
        loop {
            let next = tokio::select! {
                _ = tx.closed() => { timing.client_gone=true; break; },
                item = tokio::time::timeout(idle_timeout, stream.next()) => item,
            };
            let item = match next {
                Ok(Some(item)) => item,
                // Upstream finished (stream exhausted).
                Ok(None) => break,
                // No byte for `idle_timeout`: abort like a transport error so
                // the client stops waiting and `finish` releases the lease.
                Err(_elapsed) => {
                    let detail = format!(
                        "upstream idle timeout: no bytes for {}s",
                        idle_timeout.as_secs_f64()
                    );
                    error = Some(detail.clone());
                    let _ = tx.send(Err(std::io::Error::other(detail))).await;
                    break;
                }
            };
            match item {
                Ok(chunk) => {
                    timing.on_chunk();
                    for event in events.push(&chunk) {
                        if native_responses
                            && event
                                .lines()
                                .filter_map(|l| l.strip_prefix("data:"))
                                .filter_map(|d| {
                                    serde_json::from_str::<serde_json::Value>(d.trim()).ok()
                                })
                                .any(|v| {
                                    matches!(
                                        v["type"].as_str(),
                                        Some(
                                            "response.completed"
                                                | "response.failed"
                                                | "response.incomplete"
                                        )
                                    )
                                })
                        {
                            terminal = true;
                        }
                        timing.on_payload(event.as_bytes());
                        usage.observe(&event);
                    }
                    // Backpressure: bounded channel; client disconnect drops
                    // the receiver and we stop polling upstream. Send FIRST,
                    // then observe — the copies never delay the client.
                    if tx.send(Ok(chunk.clone())).await.is_err() {
                        timing.client_gone = true;
                        break;
                    }
                    capture(&mut captured, &chunk, capture_limit);
                    raw_captured.push(&chunk);
                }
                Err(err) => {
                    error = Some(err.to_string());
                    let _ = tx.send(Err(std::io::Error::other(err))).await;
                    break;
                }
            }
        }
        timing.on_stream_end();
        if let Some(rest) = events.take_remainder() {
            timing.on_payload(rest.as_bytes());
            usage.observe(&rest);
        }
        if native_responses && !terminal && !timing.client_gone {
            timing.saw_error_event = true;
            if error.is_none() {
                error = Some("Responses stream ended without a terminal event".into());
            }
        }
        finish(usage.finish(), captured, raw_captured, error, timing);
    });
    axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE_START: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}";
    const MESSAGE_DELTA: &str =
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":42}}";

    #[test]
    fn whole_event_in_one_chunk() {
        let mut buf = EventBuffer::new();
        let events = buf.push(format!("{MESSAGE_START}\n\n").as_bytes());
        assert_eq!(events, vec![MESSAGE_START.to_string()]);
    }

    #[test]
    fn event_split_mid_line_across_chunks() {
        let whole = format!("{MESSAGE_START}\n\n");
        // Split in the middle of the JSON payload (mid-line).
        let (a, b) = whole.split_at(whole.len() / 2);
        let mut buf = EventBuffer::new();
        assert!(
            buf.push(a.as_bytes()).is_empty(),
            "incomplete event stays buffered"
        );
        assert_eq!(buf.push(b.as_bytes()), vec![MESSAGE_START.to_string()]);
    }

    #[test]
    fn event_split_mid_terminator() {
        // The "\n\n" terminator itself fragments across chunks.
        let mut buf = EventBuffer::new();
        assert!(buf.push(format!("{MESSAGE_DELTA}\n").as_bytes()).is_empty());
        assert_eq!(buf.push(b"\n"), vec![MESSAGE_DELTA.to_string()]);
    }

    #[test]
    fn multiple_events_in_one_chunk_plus_partial_tail() {
        let chunk = format!("{MESSAGE_START}\n\n{MESSAGE_DELTA}\n\nevent: partial\ndata: {{");
        let mut buf = EventBuffer::new();
        let events = buf.push(chunk.as_bytes());
        assert_eq!(
            events,
            vec![MESSAGE_START.to_string(), MESSAGE_DELTA.to_string()]
        );
        assert_eq!(
            buf.take_remainder(),
            Some("event: partial\ndata: {".to_string())
        );
    }

    #[test]
    fn one_byte_at_a_time_still_yields_the_event() {
        let whole = format!("{MESSAGE_DELTA}\n\n");
        let mut buf = EventBuffer::new();
        let mut events = Vec::new();
        for byte in whole.as_bytes() {
            events.extend(buf.push(&[*byte]));
        }
        assert_eq!(events, vec![MESSAGE_DELTA.to_string()]);
    }

    #[test]
    fn blank_only_events_are_skipped() {
        let mut buf = EventBuffer::new();
        assert!(buf.push(b"\n\n\n\n").is_empty());
        assert_eq!(buf.take_remainder(), None);
    }

    /// `MESSAGE_START` / `MESSAGE_DELTA` with CRLF line endings, the framing
    /// WHATWG HTML §9.2.6 allows next to LF and lone CR.
    fn crlf(event: &str) -> String {
        event.replace('\n', "\r\n")
    }

    #[test]
    fn crlf_events_are_split_and_handed_over_with_lf_lines() {
        let chunk = format!(
            "{}\r\n\r\n{}\r\n\r\n",
            crlf(MESSAGE_START),
            crlf(MESSAGE_DELTA)
        );
        let mut buf = EventBuffer::new();
        let events = buf.push(chunk.as_bytes());
        assert_eq!(
            events,
            vec![MESSAGE_START.to_string(), MESSAGE_DELTA.to_string()],
            "same events, `\\r` stripped, as the LF stream yields"
        );
        assert_eq!(buf.take_remainder(), None);
        // The parsers see exactly what they see on an LF stream.
        assert_eq!(extract_usage(&events[1]), extract_usage(MESSAGE_DELTA));
    }

    #[test]
    fn crlf_terminator_split_across_pushes_at_every_byte() {
        let whole = format!("{}\r\n\r\n", crlf(MESSAGE_DELTA));
        let bytes = whole.as_bytes();
        // Every split point, including between the `\r` and `\n` of each
        // half of the terminator.
        for cut in 1..bytes.len() {
            let mut buf = EventBuffer::new();
            let mut events = buf.push(&bytes[..cut]);
            events.extend(buf.push(&bytes[cut..]));
            assert_eq!(events, vec![MESSAGE_DELTA.to_string()], "cut at {cut}");
            assert_eq!(buf.take_remainder(), None, "cut at {cut}");
        }
        // And one byte at a time.
        let mut buf = EventBuffer::new();
        let mut events = Vec::new();
        for byte in bytes {
            events.extend(buf.push(&[*byte]));
        }
        assert_eq!(events, vec![MESSAGE_DELTA.to_string()]);
    }

    #[test]
    fn mixed_line_endings_terminate_events() {
        // `\r\n\n`, `\n\r\n` and lone-CR `\r\r` are all a line end
        // followed by a blank line.
        // The next event's first byte follows, so a trailing `\r` is decided.
        for terminator in ["\r\n\n", "\n\r\n", "\r\r", "\r\n\r\n", "\n\n"] {
            let chunk =
                format!("{MESSAGE_DELTA}{terminator}{MESSAGE_START}{terminator}event: tail");
            let mut buf = EventBuffer::new();
            assert_eq!(
                buf.push(chunk.as_bytes()),
                vec![MESSAGE_DELTA.to_string(), MESSAGE_START.to_string()],
                "{terminator:?}"
            );
            assert_eq!(
                buf.take_remainder(),
                Some("event: tail".to_string()),
                "{terminator:?}"
            );
        }
        // A trailing lone `\r` is held until the next byte decides it.
        let mut buf = EventBuffer::new();
        assert!(buf
            .push(format!("{MESSAGE_DELTA}\r\n\r").as_bytes())
            .is_empty());
        assert_eq!(buf.push(b"\n"), vec![MESSAGE_DELTA.to_string()]);
        // An unterminated CRLF tail comes back LF-normalized.
        let mut buf = EventBuffer::new();
        assert!(buf
            .push(crlf("event: partial\ndata: {").as_bytes())
            .is_empty());
        assert_eq!(
            buf.take_remainder(),
            Some("event: partial\ndata: {".to_string())
        );
    }

    #[test]
    fn extract_usage_message_start() {
        assert_eq!(
            extract_usage(MESSAGE_START),
            Some(StreamUsage {
                input_tokens: 25,
                output_tokens: 0,
                // No cache keys in the payload → unavailable, not zero.
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                cache_creation_extended: None,
                web_search_requests: None,
                sub_usage: Vec::new(),
                served_model: None,
                unbilled_refusal: false,
            })
        );
    }

    #[test]
    fn extract_usage_message_start_captures_cache_fields() {
        let event = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2679,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":40000,\"output_tokens\":3}}}";
        assert_eq!(
            extract_usage(event),
            Some(StreamUsage {
                input_tokens: 2679,
                output_tokens: 0,
                // Present in the payload → captured (explicit 0 stays Some(0)).
                cache_read_input_tokens: Some(40000),
                cache_creation_input_tokens: Some(0),
                cache_creation_extended: None,
                web_search_requests: None,
                sub_usage: Vec::new(),
                served_model: None,
                unbilled_refusal: false,
            })
        );
    }

    #[test]
    fn extract_usage_message_delta() {
        assert_eq!(
            extract_usage(MESSAGE_DELTA),
            Some(StreamUsage {
                input_tokens: 0,
                output_tokens: 42,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                cache_creation_extended: None,
                web_search_requests: None,
                sub_usage: Vec::new(),
                served_model: None,
                unbilled_refusal: false,
            })
        );
    }

    #[test]
    fn extract_usage_ignores_other_events_and_malformed_json() {
        assert_eq!(
            extract_usage("event: content_block_delta\ndata: {\"type\":\"content_block_delta\"}"),
            None
        );
        assert_eq!(extract_usage("data: {not json"), None);
        assert_eq!(extract_usage("event: ping"), None);
        assert_eq!(
            extract_usage("data: {\"type\":\"message_start\",\"message\":{}}"),
            None,
            "missing usage payload is tolerated"
        );
    }

    #[test]
    fn native_responses_usage_treats_cache_writes_as_part_of_input() {
        // `run --codex` passthrough: OpenAI's `cached_tokens` and
        // `cache_write_tokens` are both subsets of `input_tokens`, so fresh
        // input is what is left after both, and the writes are cache
        // creation (they were dropped before).
        let event = "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":12000,\"input_tokens_details\":{\"cached_tokens\":4000,\"cache_write_tokens\":7000},\"output_tokens\":10}}}";
        assert_eq!(
            extract_usage(event),
            Some(StreamUsage {
                input_tokens: 1_000,
                output_tokens: 10,
                cache_read_input_tokens: Some(4_000),
                cache_creation_input_tokens: Some(7_000),
                cache_creation_extended: None,
                web_search_requests: None,
                sub_usage: Vec::new(),
                served_model: None,
                unbilled_refusal: false,
            })
        );
        // No write field: unchanged behaviour (creation unavailable).
        let event = "data: {\"type\":\"response.failed\",\"response\":{\"usage\":{\"input_tokens\":50,\"input_tokens_details\":{\"cached_tokens\":20},\"output_tokens\":3}}}";
        assert_eq!(
            extract_usage(event),
            Some(StreamUsage {
                input_tokens: 30,
                output_tokens: 3,
                cache_read_input_tokens: Some(20),
                cache_creation_input_tokens: None,
                cache_creation_extended: None,
                web_search_requests: None,
                sub_usage: Vec::new(),
                served_model: None,
                unbilled_refusal: false,
            })
        );
    }

    #[test]
    fn usage_accumulates() {
        let mut total = StreamUsage::default();
        total.add(StreamUsage {
            input_tokens: 10,
            output_tokens: 0,
            cache_read_input_tokens: Some(5),
            cache_creation_input_tokens: None,
            cache_creation_extended: None,
            web_search_requests: None,
            sub_usage: Vec::new(),
            served_model: None,
            unbilled_refusal: false,
        });
        total.add(StreamUsage {
            input_tokens: 0,
            output_tokens: 7,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            cache_creation_extended: None,
            web_search_requests: None,
            sub_usage: Vec::new(),
            served_model: None,
            unbilled_refusal: false,
        });
        assert_eq!(
            total,
            StreamUsage {
                input_tokens: 10,
                output_tokens: 7,
                // cache_read carried from the first observation; cache_creation
                // never reported → stays unavailable.
                cache_read_input_tokens: Some(5),
                cache_creation_input_tokens: None,
                cache_creation_extended: None,
                web_search_requests: None,
                sub_usage: Vec::new(),
                served_model: None,
                unbilled_refusal: false,
            }
        );
    }

    #[test]
    fn add_opt_is_present_iff_either_side_is() {
        assert_eq!(add_opt(None, None), None);
        assert_eq!(add_opt(Some(3), None), Some(3));
        assert_eq!(add_opt(None, Some(4)), Some(4));
        assert_eq!(add_opt(Some(3), Some(4)), Some(7));
    }
    fn tokens(input: u64, output: u64, read: Option<u64>, write: Option<u64>) -> TokenCounts {
        TokenCounts {
            input,
            output,
            cache_read: read,
            cache_creation: write,
            cache_creation_extended: None,
            web_search_requests: None,
        }
    }

    /// The advisor-tool example from platform.claude.com/docs/en/agents-and-
    /// tools/tool-use/advisor-tool ("Usage and billing", read 2026-11-11), as
    /// the final `message_delta` that carries it on a stream.
    const ADVISOR_DELTA: &str = r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":1760,"cache_read_input_tokens":412,"cache_creation_input_tokens":0,"output_tokens":531,"iterations":[{"type":"message","input_tokens":412,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":89},{"type":"advisor_message","model":"claude-opus-5-5","input_tokens":823,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":1612},{"type":"message","input_tokens":1348,"cache_read_input_tokens":412,"cache_creation_input_tokens":0,"output_tokens":442}]}}"#;

    #[test]
    fn advisor_iteration_is_sub_usage_and_the_top_level_is_unchanged() {
        let mut observer = UsageObserver::default();
        observer.observe(MESSAGE_START);
        observer.observe(ADVISOR_DELTA);
        let usage = observer.finish();
        // Top-level executor counters exactly as before.
        assert_eq!(usage.input_tokens, 1760);
        assert_eq!(usage.output_tokens, 531);
        assert_eq!(usage.cache_read_input_tokens, Some(412));
        assert_eq!(usage.cache_creation_input_tokens, Some(0));
        // The advisor is not in them: it rides along under its own model.
        assert_eq!(
            usage.sub_usage,
            vec![SubUsage {
                model: Some("claude-opus-5-5".into()),
                tokens: tokens(823, 1612, Some(0), Some(0)),
                price_class: crate::pricing::PriceClass::STANDARD,
            }]
        );
        assert_eq!(usage.served_model, None, "no fallback served this turn");
    }

    #[test]
    fn compaction_iteration_names_no_model_and_message_entries_are_covered() {
        // build-with-claude/compaction-threshold ("Understanding usage",
        // read 2026-11-11): top-level = the non-compaction iterations.
        let usage = serde_json::json!({
            "input_tokens": 23000, "output_tokens": 1000,
            "iterations": [
                {"type": "compaction", "input_tokens": 180000, "output_tokens": 3500},
                {"type": "message", "input_tokens": 23000, "output_tokens": 1000},
            ],
        });
        assert_eq!(
            uncounted_iterations(&usage, &[]),
            Some(vec![SubUsage {
                model: None,
                tokens: tokens(180_000, 3_500, None, None),
                price_class: crate::pricing::PriceClass::STANDARD,
            }])
        );
        // Every response carries `iterations` once the beta is on; without a
        // compaction nothing is extra.
        let plain = serde_json::json!({"iterations": [
            {"type": "message", "input_tokens": 5, "output_tokens": 1}]});
        assert_eq!(uncounted_iterations(&plain, &[]), Some(Vec::new()));
        assert_eq!(
            uncounted_iterations(&serde_json::json!({"input_tokens": 5}), &[]),
            None
        );
    }

    /// The doc's fallback example (refusals-and-fallback, read 2026-11-11):
    /// `claude-fable-5` declined before any output, `claude-opus-4-8` served.
    fn fallback_usage(declined_output: u64) -> serde_json::Value {
        serde_json::json!({
            "input_tokens": 412, "output_tokens": 264,
            "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0,
            "iterations": [
                {"type": "message", "model": "claude-fable-5", "input_tokens": 535,
                 "output_tokens": declined_output, "cache_read_input_tokens": 0,
                 "cache_creation_input_tokens": 0},
                {"type": "fallback_message", "model": "claude-opus-4-8", "input_tokens": 412,
                 "output_tokens": 264, "cache_read_input_tokens": 0,
                 "cache_creation_input_tokens": 0},
            ],
        })
    }

    fn hop(from: Option<&str>, category: Option<&str>) -> FallbackHop {
        FallbackHop {
            from_model: from.map(str::to_string),
            to_model: Some("claude-opus-4-8".into()),
            category: category.map(str::to_string),
        }
    }

    #[test]
    fn a_declined_fallback_attempt_is_booked_only_when_anthropic_bills_it() {
        // "How refusals are billed": a refusal before any output is billed
        // only for the bio / frontier_llm / reasoning_extraction categories;
        // a mid-stream one (it streamed output) always. The served attempt
        // (`fallback_message`) is the top level and never extra.
        let declined = |output: u64| {
            Some(vec![SubUsage {
                model: Some("claude-fable-5".into()),
                tokens: tokens(535, output, Some(0), Some(0)),
                price_class: crate::pricing::PriceClass::STANDARD,
            }])
        };
        let pre_output = fallback_usage(0);
        assert_eq!(
            uncounted_iterations(&pre_output, &[hop(Some("claude-fable-5"), Some("cyber"))]),
            Some(Vec::new()),
            "cyber before any output is not billed"
        );
        assert_eq!(
            uncounted_iterations(&pre_output, &[hop(Some("claude-fable-5"), None)]),
            Some(Vec::new()),
            "a null category is not billed"
        );
        for category in BILLED_REFUSAL_CATEGORIES {
            assert_eq!(
                uncounted_iterations(&pre_output, &[hop(Some("claude-fable-5"), Some(category))]),
                declined(0),
                "{category}"
            );
        }
        // Paired by position when the one hop and the one attempt cannot be
        // matched by model (an alias echoed in `from.model`).
        assert_eq!(
            uncounted_iterations(&pre_output, &[hop(Some("fable-alias"), Some("bio"))]),
            declined(0)
        );
        // No category seen at all: only output decides.
        assert_eq!(uncounted_iterations(&pre_output, &[]), Some(Vec::new()));
        let mid_stream = fallback_usage(120);
        assert_eq!(
            uncounted_iterations(&mid_stream, &[hop(Some("claude-fable-5"), Some("cyber"))]),
            declined(120),
            "a mid-stream refusal bills its input and the output already streamed"
        );
        assert_eq!(uncounted_iterations(&mid_stream, &[]), declined(120));
    }

    #[test]
    fn a_fallback_served_turn_is_booked_under_the_model_that_served_it() {
        // "The top-level model field reports the model that produced the
        // returned message". A mid-output decline: message_start still names
        // the requested model, so the served one comes from the
        // `fallback_message` entry (and the `fallback` block's `to.model`).
        let start = r#"data: {"type":"message_start","message":{"model":"claude-fable-5","usage":{"input_tokens":535,"output_tokens":1}}}"#;
        let block = r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"fallback","from":{"model":"claude-fable-5"},"to":{"model":"claude-opus-4-8"},"trigger":{"type":"refusal","category":"cyber"}}}"#;
        let delta = format!(
            "data: {}",
            serde_json::json!({"type": "message_delta", "usage": fallback_usage(120)})
        );
        let mut observer = UsageObserver::default();
        for event in [start, block, delta.as_str()] {
            observer.observe(event);
        }
        let usage = observer.finish();
        assert_eq!((usage.input_tokens, usage.output_tokens), (412, 264));
        assert_eq!(usage.served_model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(
            usage.sub_usage,
            vec![SubUsage {
                model: Some("claude-fable-5".into()),
                tokens: tokens(535, 120, Some(0), Some(0)),
                price_class: crate::pricing::PriceClass::STANDARD,
            }]
        );
        // The reported model is the fallback when nothing better names it.
        assert_eq!(
            fallback_served_model(
                &serde_json::json!({"iterations": [{"type": "fallback_message", "input_tokens": 1}]}),
                &[],
                Some("claude-opus-4-8"),
            )
            .as_deref(),
            Some("claude-opus-4-8")
        );
        // A turn the requested model served keeps its attribution.
        assert_eq!(
            fallback_served_model(
                &serde_json::json!({"iterations": [{"type": "message", "input_tokens": 1}]}),
                &[],
                Some("claude-fable-5"),
            ),
            None
        );
        assert_eq!(
            fallback_served_model(&serde_json::Value::Null, &[], Some("m")),
            None
        );
    }

    #[test]
    fn a_fallback_stream_takes_its_extended_writes_from_the_serving_attempt_too() {
        // The top level of a fallback-served turn is the `fallback_message`
        // entry, its TTL split included: message_start's split belongs to
        // the declining attempt and must not ride along.
        let start = r#"data: {"type":"message_start","message":{"model":"claude-opus-4-8","usage":{"input_tokens":535,"cache_creation_input_tokens":900,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":900},"output_tokens":1}}}"#;
        let delta = |served: &str| {
            format!(
                r#"data: {{"type":"message_delta","usage":{{"output_tokens":264,"iterations":[{{"type":"message","model":"claude-fable-5","input_tokens":535,"cache_creation_input_tokens":900,"cache_creation":{{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":900}},"output_tokens":0}},{{"type":"fallback_message","model":"claude-opus-4-8","input_tokens":412,{served}"output_tokens":264}}]}}}}"#
            )
        };
        let run = |served: &str| {
            let mut observer = UsageObserver::default();
            observer.observe(start);
            observer.observe(&delta(served));
            observer.finish()
        };
        let usage = run(
            r#""cache_creation_input_tokens":300,"cache_creation":{"ephemeral_5m_input_tokens":100,"ephemeral_1h_input_tokens":200},"#,
        );
        assert_eq!(
            (
                usage.cache_creation_input_tokens,
                usage.cache_creation_extended
            ),
            (Some(300), Some(200)),
            "the serving attempt's split"
        );
        let usage = run(r#""cache_creation_input_tokens":300,"#);
        assert_eq!(
            (
                usage.cache_creation_input_tokens,
                usage.cache_creation_extended
            ),
            (Some(300), None),
            "no split on the serving attempt: none, not the declined one's"
        );
    }

    #[test]
    fn a_fallback_stream_takes_its_top_level_counters_from_the_serving_attempt() {
        // A pre-output decline: message_start carries the DECLINING model's
        // usage and the final message_delta omits `input_tokens`. The doc
        // guarantees top-level == the `fallback_message` entry, so the four
        // counters come from it — the declining model's input must not stay
        // in the top level (it would be priced under the serving model AND
        // again as a sub-usage).
        let start = r#"data: {"type":"message_start","message":{"model":"claude-opus-4-8","usage":{"input_tokens":535,"cache_read_input_tokens":40,"output_tokens":1}}}"#;
        let block = r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"fallback","from":{"model":"claude-fable-5"},"to":{"model":"claude-opus-4-8"},"trigger":{"type":"refusal","category":"bio"}}}"#;
        let delta = r#"data: {"type":"message_delta","usage":{"output_tokens":264,"iterations":[{"type":"message","model":"claude-fable-5","input_tokens":535,"cache_read_input_tokens":40,"output_tokens":0},{"type":"fallback_message","model":"claude-opus-4-8","input_tokens":412,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":264}]}}"#;
        let mut observer = UsageObserver::default();
        for event in [start, block, delta] {
            observer.observe(event);
        }
        let usage = observer.finish();
        assert_eq!(
            (
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_input_tokens,
                usage.cache_creation_input_tokens
            ),
            (412, 264, Some(0), Some(0)),
            "the serving attempt's counters, not message_start's"
        );
        assert_eq!(usage.served_model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(
            usage.sub_usage,
            vec![SubUsage {
                model: Some("claude-fable-5".into()),
                tokens: tokens(535, 0, Some(40), None),
                price_class: crate::pricing::PriceClass::STANDARD,
            }],
            "the billed (bio) declined attempt, once"
        );
    }

    #[test]
    fn unknown_iteration_types_count_and_empty_or_untyped_entries_do_not() {
        let usage = serde_json::json!({"iterations": [
            {"type": "message", "input_tokens": 9, "output_tokens": 9},
            {"type": "some_future_kind", "model": "claude-x", "input_tokens": 7, "output_tokens": 3},
            {"type": "advisor_message", "model": "claude-y", "input_tokens": 0, "output_tokens": 0},
            {"input_tokens": 4, "output_tokens": 4},
        ]});
        assert_eq!(
            uncounted_iterations(&usage, &[]),
            Some(vec![SubUsage {
                model: Some("claude-x".into()),
                tokens: tokens(7, 3, None, None),
                price_class: crate::pricing::PriceClass::STANDARD,
            }])
        );
    }

    #[test]
    fn iteration_snapshots_replace_each_other_and_survive_a_later_delta_without_them() {
        let start = r#"data: {"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":1,"iterations":[{"type":"compaction","input_tokens":100,"output_tokens":10}]}}}"#;
        let delta = r#"data: {"type":"message_delta","usage":{"output_tokens":20,"iterations":[{"type":"compaction","input_tokens":100,"output_tokens":10},{"type":"advisor_message","model":"claude-z","input_tokens":30,"output_tokens":3},{"type":"message","input_tokens":10,"output_tokens":20}]}}"#;
        let late = r#"data: {"type":"message_delta","usage":{"output_tokens":21}}"#;
        let mut seeded = UsageObserver::default();
        seeded.observe(start);
        assert_eq!(seeded.finish().sub_usage.len(), 1, "message_start seeds");
        let mut observer = UsageObserver::default();
        for event in [start, delta, late] {
            observer.observe(event);
        }
        let usage = observer.finish();
        assert_eq!(
            usage.sub_usage,
            vec![
                SubUsage {
                    model: None,
                    tokens: tokens(100, 10, None, None),
                    price_class: crate::pricing::PriceClass::STANDARD,
                },
                SubUsage {
                    model: Some("claude-z".into()),
                    tokens: tokens(30, 3, None, None),
                    price_class: crate::pricing::PriceClass::STANDARD,
                },
            ],
            "the cumulative array replaces the earlier one, never doubles it"
        );
        assert_eq!(usage.output_tokens, 21);
    }

    #[test]
    fn sdk_internal_round_totals_replace_initial_usage() {
        let mut observer = UsageObserver::default();
        observer.observe(MESSAGE_START);
        observer.observe("data: {\"type\":\"message_delta\",\"usage\":{\"input_tokens\":50,\"output_tokens\":12,\"cache_read_input_tokens\":20}}");
        let usage = observer.finish();
        assert_eq!(usage.input_tokens, 50);
        assert_eq!(usage.output_tokens, 12);
        assert_eq!(usage.cache_read_input_tokens, Some(20));
    }

    /// The "1-hour cache duration" response example of
    /// platform.claude.com/docs/en/build-with-claude/prompt-caching (read
    /// 2026-11-11): `cache_creation_input_tokens` is the sum of the
    /// `cache_creation` object, 100 of the 248 written tokens at the 1h TTL.
    const SPLIT_START: &str = r#"event: message_start
data: {"type":"message_start","message":{"model":"claude-opus-5-5","usage":{"input_tokens":2048,"cache_read_input_tokens":1800,"cache_creation_input_tokens":248,"cache_creation":{"ephemeral_5m_input_tokens":148,"ephemeral_1h_input_tokens":100},"output_tokens":1}}}"#;

    #[test]
    fn the_extended_ttl_write_subset_is_read_from_message_start_and_delta() {
        let mut observer = UsageObserver::default();
        observer.observe(SPLIT_START);
        let started = observer.finish();
        assert_eq!(started.cache_creation_input_tokens, Some(248));
        assert_eq!(
            started.cache_creation_extended,
            Some(100),
            "message_start carries the 1h subset"
        );
        // `message_delta` counters are cumulative and replace the start's;
        // the split goes with its total (the SDK bridge's summed rounds).
        let delta = r#"data: {"type":"message_delta","usage":{"output_tokens":9,"cache_creation_input_tokens":548,"cache_creation":{"ephemeral_5m_input_tokens":148,"ephemeral_1h_input_tokens":400}}}"#;
        let mut observer = UsageObserver::default();
        observer.observe(SPLIT_START);
        observer.observe(delta);
        let usage = observer.finish();
        assert_eq!(
            (
                usage.cache_creation_input_tokens,
                usage.cache_creation_extended
            ),
            (Some(548), Some(400)),
            "the delta's split replaces the start's, never adds to it"
        );
        // A delta without a split keeps the start's.
        let mut observer = UsageObserver::default();
        observer.observe(SPLIT_START);
        observer.observe(r#"data: {"type":"message_delta","usage":{"output_tokens":9}}"#);
        assert_eq!(observer.finish().cache_creation_extended, Some(100));
        // No split at all: unknown, not zero (priced exactly as before).
        let mut observer = UsageObserver::default();
        observer.observe(MESSAGE_START);
        assert_eq!(observer.finish().cache_creation_extended, None);
    }

    #[test]
    fn a_malformed_split_is_clamped_to_the_total() {
        let start = r#"data: {"type":"message_start","message":{"usage":{"input_tokens":1,"cache_creation_input_tokens":10,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":99}}}}"#;
        let usage = extract_usage(start).expect("usage");
        assert_eq!(
            (
                usage.cache_creation_input_tokens,
                usage.cache_creation_extended
            ),
            (Some(10), Some(10))
        );
        // A delta that lowers the total below an earlier split re-clamps.
        let mut observer = UsageObserver::default();
        observer.observe(SPLIT_START);
        observer.observe(
            r#"data: {"type":"message_delta","usage":{"output_tokens":1,"cache_creation_input_tokens":50}}"#,
        );
        let usage = observer.finish();
        assert_eq!(
            (
                usage.cache_creation_input_tokens,
                usage.cache_creation_extended
            ),
            (Some(50), Some(50))
        );
    }

    #[test]
    fn native_responses_usage_reads_the_12h_write_subset() {
        // The worked example of developers.openai.com/api/docs/guides/
        // prompt-caching (read 2026-11-11): 12,000 input, 4,000 cached,
        // 7,000 written — 3,000 at 30m and 4,000 at 12h
        // (`input_tokens_details.cache_write_tokens_details.ttl_12h_tokens`).
        let event = "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":12000,\"input_tokens_details\":{\"cached_tokens\":4000,\"cache_write_tokens\":7000,\"cache_write_tokens_details\":{\"ttl_30m_tokens\":3000,\"ttl_12h_tokens\":4000}},\"output_tokens\":10}}}";
        let usage = extract_usage(event).expect("usage");
        assert_eq!(usage.input_tokens, 1_000);
        assert_eq!(usage.cache_creation_input_tokens, Some(7_000));
        assert_eq!(usage.cache_creation_extended, Some(4_000));
    }

    #[test]
    fn web_search_requests_are_read_and_cumulative_deltas_replace_them() {
        // platform.claude.com/docs/en/about-claude/pricing ("Web search
        // tool", read 2026-11-11): `usage.server_tool_use.web_search_requests`.
        let start = r#"data: {"type":"message_start","message":{"usage":{"input_tokens":105,"output_tokens":1,"server_tool_use":{"web_search_requests":1}}}}"#;
        let delta = r#"data: {"type":"message_delta","usage":{"output_tokens":6039,"server_tool_use":{"web_search_requests":2}}}"#;
        let mut observer = UsageObserver::default();
        observer.observe(start);
        assert_eq!(observer.finish().web_search_requests, Some(1));
        let mut observer = UsageObserver::default();
        observer.observe(start);
        observer.observe(delta);
        assert_eq!(
            observer.finish().web_search_requests,
            Some(2),
            "the cumulative delta replaces the start's count"
        );
        let mut observer = UsageObserver::default();
        observer.observe(MESSAGE_START);
        assert_eq!(observer.finish().web_search_requests, None);
    }

    #[test]
    fn a_sub_usage_carries_its_own_extended_writes() {
        // prompt-caching: the `cache_creation` object also sits inside each
        // `iterations` entry.
        let usage = serde_json::json!({"iterations": [
            {"type": "message", "input_tokens": 9, "output_tokens": 9},
            {"type": "advisor_message", "model": "claude-opus-5-5", "input_tokens": 7,
             "output_tokens": 3, "cache_creation_input_tokens": 500,
             "cache_creation": {"ephemeral_5m_input_tokens": 200, "ephemeral_1h_input_tokens": 300}},
        ]});
        let subs = uncounted_iterations(&usage, &[]).expect("iterations");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].tokens.cache_creation, Some(500));
        assert_eq!(subs[0].tokens.cache_creation_extended, Some(300));
    }

    /// #283: an Anthropic refusal before any output is not billed unless its
    /// `stop_details.category` is billed ("How refusals are billed",
    /// build-with-claude/refusals-and-fallback, read 2026-11-11); a refusal
    /// after streamed output keeps its price. The tokens stay recorded.
    #[test]
    fn a_refusal_before_any_output_in_an_unbilled_category_is_marked() {
        let start = r#"event: message_start
data: {"type":"message_start","message":{"model":"claude-fable-5-2","usage":{"input_tokens":412,"cache_read_input_tokens":100,"output_tokens":0}}}"#
            .to_string();
        let refusal = |category: &str, output: u64| {
            format!(
                r#"event: message_delta
data: {{"type":"message_delta","delta":{{"stop_reason":"refusal","stop_sequence":null,"stop_details":{{"type":"refusal","category":{category},"explanation":null}}}},"usage":{{"output_tokens":{output}}}}}"#
            )
        };
        let text = r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Sure"}}"#
            .to_string();
        let run = |events: &[String]| {
            let mut observer = UsageObserver::default();
            for event in events {
                observer.observe(event);
            }
            observer.finish()
        };
        for category in [r#""cyber""#, r#""general_harms""#, "null"] {
            let usage = run(&[start.clone(), refusal(category, 0)]);
            assert!(usage.unbilled_refusal, "{category}");
            assert_eq!(
                (usage.input_tokens, usage.cache_read_input_tokens),
                (412, Some(100)),
                "the tokens stay visible"
            );
        }
        for category in BILLED_REFUSAL_CATEGORIES {
            let usage = run(&[start.clone(), refusal(&format!("\"{category}\""), 0)]);
            assert!(!usage.unbilled_refusal, "{category} is billed");
        }
        // Mid-stream: output came first, so it bills like any request.
        assert!(!run(&[start.clone(), text.clone(), refusal(r#""cyber""#, 25)]).unbilled_refusal);
        assert!(!run(&[start.clone(), text, refusal(r#""cyber""#, 0)]).unbilled_refusal);
        let end = r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":0}}"#
            .to_string();
        assert!(!run(&[start, end]).unbilled_refusal);
    }

    /// #283 review: the refusal rule applies to the attempt whose counters
    /// the request is priced on. A first attempt that streamed text and then
    /// declined hands over to a fallback model (a `fallback` block); when
    /// that final attempt refuses before any output in an unbilled category,
    /// the request's top-level counters (its `fallback_message` entry) are
    /// unbilled — the earlier attempt's text does not make them billed (the
    /// declined attempt itself is booked as a sub-usage, billed because it
    /// streamed output).
    #[test]
    fn the_refusal_rule_follows_the_attempt_the_counters_come_from() {
        let events = [
            r#"data: {"type":"message_start","message":{"model":"claude-fable-5-2","usage":{"input_tokens":900,"output_tokens":0}}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Here is"}}"#,
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"fallback","from":{"model":"claude-fable-5-2"},"to":{"model":"claude-opus-5-5"},"trigger":{"type":"refusal","category":"cyber"}}}"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"}},"usage":{"output_tokens":0,"iterations":[{"type":"message","model":"claude-fable-5-2","input_tokens":900,"output_tokens":5},{"type":"fallback_message","model":"claude-opus-5-5","input_tokens":950,"output_tokens":0}]}}"#,
        ];
        let mut observer = UsageObserver::default();
        for event in events {
            observer.observe(event);
        }
        let usage = observer.finish();
        assert_eq!((usage.input_tokens, usage.output_tokens), (950, 0));
        assert!(usage.unbilled_refusal, "{usage:?}");
        assert_eq!(usage.sub_usage.len(), 1, "the declined attempt is billed");
        // Text the FINAL attempt streamed still makes it billed.
        let mut observer = UsageObserver::default();
        for event in [events[0], events[2], events[1], events[3]] {
            observer.observe(event);
        }
        assert!(!observer.finish().unbilled_refusal);
    }
}
