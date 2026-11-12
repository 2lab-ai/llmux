//! API-equivalent USD reference pricing for token usage (Feature D).
//!
//! The proxy bills nothing. Independent USD/MTok rates describe fresh input,
//! output, cache reads, cache writes and (#278) extended-TTL cache writes —
//! Anthropic 1h, OpenAI 12h, priced on the subset a request reported
//! ([`TokenCounts::cache_creation_extended`]); provider/model-specific rates
//! must not be inferred from a universal multiplier. Anthropic web searches
//! add a per-search fee ([`ANTHROPIC_WEB_SEARCH_USD`]).
//!
//! ## Price classes (#278, #283)
//!
//! Which rates apply depends on facts of the ONE request, not of any total:
//! its prompt size (long-context tiers: OpenAI `> 272,000`, xAI `>= 200,000`,
//! Anthropic Haiku 5.5 `> 100,000` prompt tokens bill the WHOLE request at
//! the long rates), the codex Fast service tier, the UTC day it ran on (dated
//! rate changes: Gemini 4 Argon from 2026-12-01, Gemini 3.8 Flash from
//! 2027-01-01) and whether it was an Anthropic refusal that is not billed. A
//! request's [`PriceClass`] is resolved ONCE, when the request is recorded
//! ([`PriceClass::resolve`]), from those facts and built-in data only (the
//! boundaries and the change dates are not configurable), and travels with
//! the request: the activity log, every aggregate and the key-usage store
//! keep their tokens per class ([`ClassSplit`]), so no aggregate prices a
//! mixed total and **aggregate cost == sum of per-request costs**. A request
//! recorded with no class (every line written before classes existed) is the
//! standard class and prices exactly as before.
//!
//! All rates are **USD per 1,000,000 tokens**. Rates sourced: claude-api skill
//! cached 2026-06-04; OpenAI gpt-5.5 pricing 2026-04-23; Opus 5.5 from
//! anthropic.com/claude-opus-5-5, 2026-09-22; Sonnet 5 / 5.5 and the gpt-6 /
//! gpt-6.1 family from platform.claude.com/docs/en/about-claude/pricing and
//! developers.openai.com/api/docs/pricing, both read 2026-10-06 (that read
//! also corrected the stale gpt-5.6-sol / gpt-5.6-luna rows below). Haiku 5.5,
//! Sonnet 5.5 and Fable 5.1 cache rates re-read 2026-10-08 at
//! https://platform.claude.com/docs/en/about-claude/pricing. Gemini 3.8 Flash
//! and Gemini 4 Argon from https://ai.google.dev/gemini-api/docs/pricing
//! (updated 2026-10-14). gpt-5.6-terra and grok-4.5 corrected 2026-10-23
//! against their model pages (see the rows). Fable 5.2 from
//! https://platform.claude.com/docs/en/about-claude/pricing and
//! gpt-6.1-terra from https://developers.openai.com/api/docs/models/gpt-6.1-terra
//! (and the pricing page), both read 2026-11-10. Mythos 5.2 / 5.1 / 5 rows and
//! the gpt-5.6-terra / gpt-5.6-luna cache-write rates read 2026-11-11 (see
//! the rows). The long-context and Fast tiers, the Opus 4.1 / 4 rows and the
//! dated Gemini rates read 2026-11-11 (see the rows).

use std::collections::HashMap;

use crate::tui::activity::normalize_model;
use crate::tui::TokenCounts;

/// Per-model price table entry. All rates are **USD per 1,000,000 tokens**.
/// A zero rate means "free / not charged". A row whose provider has no
/// separate cache-write charge carries its INPUT rate as `cache_creation`
/// (written tokens are not in fresh input, so 0 would make them free).
///
/// Also the config `pricing` override shape. An override REPLACES the whole
/// row: its own standard rates, and a long-context or Fast tier only if it
/// sets one (`long_context` / `fast` / `fast_long_context`) — without them a
/// long or Fast request prices at the override's standard rates. The
/// long-context boundary and the dated rate changes are built-in data
/// (`#[serde(skip)]`): an override never carries them, so it applies its long
/// rates at the model's built-in boundary and its rates on every day.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelPrice {
    /// Fresh (non-cached) input tokens, USD / 1e6.
    pub input: f64,
    /// Output (completion) tokens, USD / 1e6.
    pub output: f64,
    /// Cache-read tokens, USD / 1e6.
    pub cache_read: f64,
    /// Cache-creation (write) tokens at the default (short) TTL — Anthropic
    /// 5 minutes, OpenAI 30 minutes — USD / 1e6.
    pub cache_creation: f64,
    /// Cache writes at the EXTENDED TTL — Anthropic 1 hour, OpenAI 12 hours —
    /// USD / 1e6, applied to [`TokenCounts::cache_creation_extended`] (#278).
    /// `None` = the same as `cache_creation`, so a row or config override
    /// written without it prices exactly as before the split existed.
    /// Additive in config: absent when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_extended: Option<f64>,
    /// The rates of a long-context request ([`PriceClass::long_context`]) —
    /// the WHOLE request, every component. `None` = no long tier: such a
    /// request prices at the standard rates. Additive in config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_context: Option<TierRates>,
    /// The rates of a Fast (codex `service_tier: "priority"`) request
    /// ([`PriceClass::fast`]). `None` = no Fast premium. Additive in config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<TierRates>,
    /// The rates of a request that is both long-context and Fast. `None` =
    /// the `long_context` rates (no Fast premium published for that tier).
    /// Additive in config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_long_context: Option<TierRates>,
    /// Built-in only: the prompt size at which a request is long-context.
    /// Never read from or written to config (thresholds are not settable).
    #[serde(skip)]
    pub long_boundary: Option<LongBoundary>,
    /// Built-in only: the row's later rates, each from a UTC day on, oldest
    /// first. This row's own rates apply before the first change. Never read
    /// from or written to config: an override prices every day the same.
    #[serde(skip)]
    pub rate_changes: &'static [RateChange],
}

/// One tier's rates (long-context or Fast), USD per 1,000,000 tokens — the
/// same five rates as a [`ModelPrice`]'s standard ones. `deny_unknown_fields`:
/// a config that tries to set a threshold here fails loudly.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_creation: f64,
    /// Extended-TTL cache writes; `None` = this tier's `cache_creation`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_extended: Option<f64>,
}

impl TierRates {
    /// Every rate times `factor` (a Fast tier the pricing page lists as a
    /// fixed multiple of another).
    pub const fn times(self, factor: f64) -> Self {
        Self {
            input: self.input * factor,
            output: self.output * factor,
            cache_read: self.cache_read * factor,
            cache_creation: self.cache_creation * factor,
            cache_creation_extended: match self.cache_creation_extended {
                Some(rate) => Some(rate * factor),
                None => None,
            },
        }
    }

    /// The rate of extended-TTL cache writes: its own, else `cache_creation`.
    pub fn extended_write_rate(&self) -> f64 {
        self.cache_creation_extended.unwrap_or(self.cache_creation)
    }
}

/// Where a model's long-context tier starts, exactly as its pricing page
/// states it, on the request's prompt (fresh input + cache reads + cache
/// writes — llmux's `input` is fresh input only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongBoundary {
    /// Long when the prompt is MORE than this many tokens (OpenAI `> 272K`,
    /// Anthropic Haiku 5.5 "over 100,000 tokens").
    Above(u64),
    /// Long when the prompt is this many tokens OR MORE (xAI `≥ 200k`).
    AtLeast(u64),
}

impl LongBoundary {
    /// Whether a prompt of `prompt` tokens is long-context.
    pub fn is_long(self, prompt: u64) -> bool {
        match self {
            Self::Above(n) => prompt > n,
            Self::AtLeast(n) => prompt >= n,
        }
    }
}

/// A built-in row's rates from a UTC day on (a provider price change).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateChange {
    pub from: RateDay,
    pub price: ModelPrice,
}

/// A UTC calendar day, as days since 1970-01-01. Rate changes take effect at
/// a UTC day boundary unless the pricing page says otherwise (none of the
/// modeled ones does). Serialized as `"YYYY-MM-DD"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RateDay(pub u32);

impl RateDay {
    /// The UTC day of `y-m-d` (proleptic Gregorian; Howard Hinnant's
    /// algorithm). For built-in dates, which are all after 1970.
    pub const fn ymd(y: i64, m: u32, d: u32) -> Self {
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let mp = if m > 2 { m as i64 - 3 } else { m as i64 + 9 };
        let doy = (153 * mp + 2) / 5 + d as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        Self(if days < 0 { 0 } else { days as u32 })
    }

    /// The UTC day `at` falls on (day 0 for a pre-epoch clock).
    pub fn of(at: std::time::SystemTime) -> Self {
        let days = at
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() / 86_400)
            .unwrap_or(0);
        Self(u32::try_from(days).unwrap_or(u32::MAX))
    }

    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.splitn(3, '-');
        let y: i64 = parts.next()?.parse().ok()?;
        let m: u32 = parts.next()?.parse().ok()?;
        let d: u32 = parts.next()?.parse().ok()?;
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) || y < 1970 {
            return None;
        }
        let day = Self::ymd(y, m, d);
        // Reject a day that does not exist (2026-02-30 would roll over).
        (day.to_string() == format!("{y:04}-{m:02}-{d:02}")).then_some(day)
    }
}

impl std::fmt::Display for RateDay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (y, m, d) = crate::tui::format::civil_from_days(i64::from(self.0));
        write!(f, "{y:04}-{m:02}-{d:02}")
    }
}

impl serde::Serialize for RateDay {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for RateDay {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("not a YYYY-MM-DD day: {text:?}")))
    }
}

/// The facts of ONE request (or one of its sub-usages) that select its rates
/// (#278, #283). The default is the standard class: what every request
/// recorded before classes existed is, and what prices exactly as before.
/// Resolved once, when the request is recorded ([`Self::resolve`]); every
/// aggregate keeps its tokens per class ([`ClassSplit`]). Serialized with
/// every default field omitted, so the standard class is `{}`.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct PriceClass {
    /// The request's prompt crossed its model's built-in long-context
    /// boundary ([`LongBoundary`]): the whole request is billed at the long
    /// rates.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub long_context: bool,
    /// Codex Fast mode (`service_tier: "priority"`) was in effect.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
    /// The first day of the dated rate change in force on the request's UTC
    /// day (`None` = the row's original rates, or a row with no change).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rates_from: Option<RateDay>,
    /// An Anthropic refusal before any output in a category Anthropic does
    /// not bill (#283): it costs nothing, its tokens stay visible. Every
    /// other fact is cleared on such a request.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unbilled: bool,
}

impl PriceClass {
    /// The standard class.
    pub const STANDARD: Self = Self {
        long_context: false,
        fast: false,
        rates_from: None,
        unbilled: false,
    };

    /// The class of an unbilled refusal (#283).
    pub const UNBILLED: Self = Self {
        long_context: false,
        fast: false,
        rates_from: None,
        unbilled: true,
    };

    /// Whether this is the standard class.
    pub fn is_standard(&self) -> bool {
        *self == Self::STANDARD
    }

    /// Resolve the class of one request (or sub-usage) of `(group, model)`
    /// from its own facts: `tokens` (its prompt), `fast`, the UTC `day` it
    /// was recorded on, and `self` — the class the proxy marked, of which
    /// only [`Self::unbilled`] is read, and only on the claude group (the
    /// only provider with that rule). Uses built-in data only, never a config
    /// override, so every path that records a request classifies it the
    /// same way: the boundary and the dated changes of the model's built-in
    /// row (exact id, prefix, then group fallback).
    pub fn resolve(
        self,
        group: &str,
        model: &str,
        tokens: &TokenCounts,
        fast: bool,
        day: RateDay,
    ) -> Self {
        if self.unbilled && group.eq_ignore_ascii_case("claude") {
            return Self::UNBILLED;
        }
        let Some(row) = price_for(group, model, &HashMap::new()) else {
            return Self {
                fast,
                ..Self::STANDARD
            };
        };
        let change = row.change_on(day);
        let effective = change.map_or(row, |c| c.price);
        Self {
            long_context: effective
                .long_boundary
                .is_some_and(|boundary| boundary.is_long(prompt_tokens(tokens))),
            fast,
            rates_from: change.map(|c| c.from),
            unbilled: false,
        }
    }

    /// A short human label (`long context` / `fast` / `rates from
    /// 2026-12-01` / `unbilled refusal`), empty for the standard class.
    pub fn label(&self) -> String {
        if self.unbilled {
            return "unbilled refusal".to_string();
        }
        let mut parts = Vec::new();
        if self.long_context {
            parts.push("long context".to_string());
        }
        if self.fast {
            parts.push("fast".to_string());
        }
        if let Some(day) = self.rates_from {
            parts.push(format!("rates from {day}"));
        }
        parts.join(" · ")
    }
}

/// The prompt a long-context boundary is measured on: fresh input plus both
/// cache classes ("A request's prompt length counts all of its input tokens,
/// including cache reads and cache writes", Anthropic pricing page; OpenAI and
/// xAI count prompt tokens the same way).
pub fn prompt_tokens(tokens: &TokenCounts) -> u64 {
    tokens
        .input
        .saturating_add(tokens.cache_read.unwrap_or(0))
        .saturating_add(tokens.cache_creation.unwrap_or(0))
}

impl ModelPrice {
    const fn new(input: f64, output: f64, cache_read: f64, cache_creation: f64) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_creation,
            cache_creation_extended: None,
            long_context: None,
            fast: None,
            fast_long_context: None,
            long_boundary: None,
            rate_changes: &[],
        }
    }

    /// The rate of extended-TTL cache writes: its own, else `cache_creation`.
    pub fn extended_write_rate(&self) -> f64 {
        self.cache_creation_extended.unwrap_or(self.cache_creation)
    }

    /// This row with an extended-TTL cache-write rate.
    const fn with_extended(self, cache_creation_extended: f64) -> Self {
        Self {
            cache_creation_extended: Some(cache_creation_extended),
            ..self
        }
    }

    /// The row's standard rates as a [`TierRates`].
    pub const fn standard(&self) -> TierRates {
        TierRates {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_creation: self.cache_creation,
            cache_creation_extended: self.cache_creation_extended,
        }
    }

    /// The rates `class` selects on this row (an unbilled class is priced 0
    /// by the caller, not here): the dated change in force on
    /// `class.rates_from`, then its long-context and / or Fast tier — each
    /// only if the row has one, else the next simpler set.
    pub fn rates(&self, class: PriceClass) -> TierRates {
        let row = class
            .rates_from
            .and_then(|from| self.change_on(from))
            .map_or(*self, |change| change.price);
        let long = if class.long_context {
            row.long_context
        } else {
            None
        };
        match (long, class.fast) {
            (None, false) => row.standard(),
            (Some(long), false) => long,
            (None, true) => row.fast.unwrap_or(row.standard()),
            (Some(long), true) => row.fast_long_context.unwrap_or(long),
        }
    }

    /// The latest built-in rate change in force on `day`, if any.
    fn change_on(&self, day: RateDay) -> Option<&'static RateChange> {
        self.rate_changes
            .iter()
            .rev()
            .find(|change| change.from <= day)
    }

    /// This row with a long-context tier starting at `boundary`.
    const fn with_long(self, boundary: LongBoundary, long: TierRates) -> Self {
        Self {
            long_context: Some(long),
            long_boundary: Some(boundary),
            ..self
        }
    }

    /// This row with a Fast tier at `factor` times its standard rates — and
    /// its long-context rates, when it has them (set the long tier first).
    const fn with_fast(self, factor: f64) -> Self {
        Self {
            fast: Some(self.standard().times(factor)),
            fast_long_context: match self.long_context {
                Some(long) => Some(long.times(factor)),
                None => None,
            },
            ..self
        }
    }

    /// This row with a Fast tier on its standard rates only (no Fast rate is
    /// published for its long-context tier).
    const fn with_fast_standard_only(self, factor: f64) -> Self {
        Self {
            fast: Some(self.standard().times(factor)),
            ..self
        }
    }

    /// This row with later rates from the given UTC days on.
    const fn with_changes(self, rate_changes: &'static [RateChange]) -> Self {
        Self {
            rate_changes,
            ..self
        }
    }

    /// All-zero rate — the fallback for genuinely unknown (group, model) pairs.
    /// Yields `0.0` cost and never panics.
    pub const fn zero() -> Self {
        Self::new(0.0, 0.0, 0.0, 0.0)
    }
}

/// One tier's rates (USD / 1e6): input, output, cache read, cache write,
/// extended-TTL cache write (`None` = the tier's cache-write rate).
const fn tier(
    input: f64,
    output: f64,
    cache_read: f64,
    cache_creation: f64,
    ext: Option<f64>,
) -> TierRates {
    TierRates {
        input,
        output,
        cache_read,
        cache_creation,
        cache_creation_extended: ext,
    }
}

/// OpenAI's long-context boundary: "Short context: ≤272K input tokens. Long
/// context: >272K input tokens", every listed GPT row; a long request bills
/// 2x input, cached input and both cache writes and 1.5x output for the whole
/// request (https://developers.openai.com/api/docs/pricing, read 2026-11-11).
const OPENAI_LONG: LongBoundary = LongBoundary::Above(272_000);
/// xAI's: rows "(≥ 200k prompt tokens)" — "requests whose prompt reaches the
/// listed token threshold are billed at the higher rate for all tokens in the
/// request" (https://docs.x.ai/developers/pricing, read 2026-11-11).
const XAI_LONG: LongBoundary = LongBoundary::AtLeast(200_000);
/// Anthropic's, Claude Haiku 5.5 only: "a request whose prompt is over
/// 100,000 tokens pays higher prices. A request's prompt length counts all of
/// its input tokens, including cache reads and cache writes"
/// (https://platform.claude.com/docs/en/about-claude/pricing, "Long context
/// pricing", read 2026-11-11). Every other Claude row bills "the full 1M
/// token context window at standard pricing".
const HAIKU_5_5_LONG: LongBoundary = LongBoundary::Above(100_000);
/// OpenAI Fast (formerly Priority) is 2x Standard on every 6.x and 5.6 row,
/// short and long context alike — every cell of the "Fast" table of
/// https://developers.openai.com/api/docs/pricing, read 2026-11-11.
const OPENAI_FAST: f64 = 2.0;

/// Every Claude row's 1-hour cache-write rate is 2x its input rate: the "1h
/// cache writes" column of https://platform.claude.com/docs/en/about-claude/pricing,
/// read 2026-11-11 ("Cache write tokens with a 1-hour TTL are 2 times the base
/// input tokens price", build-with-claude/prompt-caching, same day).
///
/// Opus-tier rates {input 5.0, output 25.0, cache_read 0.5, cache_creation 6.25,
/// 1h 10.0} (Opus 5 / 4.8 / 4.7 / 4.6 / 4.5). Also the `group == "claude"`
/// unknown-model fallback.
const OPUS_TIER: ModelPrice = ModelPrice::new(5.0, 25.0, 0.5, 6.25).with_extended(10.0);
/// Opus 5.5 (Anthropic announcement 2026-09-22): $4 in / $20 out / cache read
/// 0.20 / cache write 5.0 — 20% below the opus tier, cache reads 60% below, so
/// it must NOT fall to the `claude-opus-` prefix fallback. 1h writes $8
/// (pricing page, read 2026-11-11).
const OPUS_5_5: ModelPrice = ModelPrice::new(4.0, 20.0, 0.20, 5.0).with_extended(8.0);
/// Claude Opus 4.1 and Claude Opus 4 (retired on the Claude API, still served
/// on some cloud platforms): $15 in / $18.75 5m writes / $30 1h writes / $1.50
/// cache hits / $75 out, https://platform.claude.com/docs/en/about-claude/pricing,
/// read 2026-11-11. Three times the Opus tier, so history recorded under
/// these ids must NOT fall to the `claude-opus-` prefix fallback.
const OPUS_4_1: ModelPrice = ModelPrice::new(15.0, 75.0, 1.50, 18.75).with_extended(30.0);
/// Sonnet-tier rates {3.0, 15.0, 0.3, 3.75, 1h 6.0} — the 4.x generation
/// (`claude-sonnet-4-6` / `-4-5`) and the `claude-sonnet-` prefix fallback.
/// 1h writes $6 (pricing page, read 2026-11-11).
const SONNET_TIER: ModelPrice = ModelPrice::new(3.0, 15.0, 0.3, 3.75).with_extended(6.0);
/// Sonnet 5 (pricing page, read 2026-10-08): $2 / $10 / $0.20 / $2.50; 1h
/// writes $4 (read 2026-11-11).
const SONNET_5: ModelPrice = ModelPrice::new(2.0, 10.0, 0.20, 2.5).with_extended(4.0);
/// Sonnet 5.5 cache reads fell to $0.10 on 2026-10-07. Other rates unchanged.
/// 1h writes $4 (pricing page, read 2026-11-11).
const SONNET_5_5: ModelPrice = ModelPrice::new(2.0, 10.0, 0.10, 2.5).with_extended(4.0);
/// Haiku 4.5 rates; also the unknown Haiku family fallback. Since 2026-10-23
/// Haiku 4.5 ids are SERVED by Haiku 5.5 (`crate::catalog::SUPERSEDED_CLAUDE_IDS`),
/// but new requests are recorded under the served `claude-haiku-5-5`, so a
/// record still carrying a 4.5 id is history 4.5 really served — priced here.
/// 1h writes $2 (pricing page, read 2026-11-11).
const HAIKU_TIER: ModelPrice = ModelPrice::new(1.0, 5.0, 0.1, 1.25).with_extended(2.0);
/// Haiku 5.5: <=100,000 prompt tokens, 5m cache writes (official pricing,
/// read 2026-10-08); 1h writes $0.20 (read 2026-11-11). A prompt over 100,000
/// tokens ([`HAIKU_5_5_LONG`]) bills the whole request at 5x ALL these rates:
/// $0.50 in / $0.625 5m writes / $1 1h writes / $0.05 cache hits / $2.50 out
/// (the "for prompts over 100,000 tokens" row, pricing page, read 2026-11-11).
const HAIKU_5_5: ModelPrice = ModelPrice::new(0.10, 0.50, 0.01, 0.125)
    .with_extended(0.20)
    .with_long(HAIKU_5_5_LONG, tier(0.50, 2.50, 0.05, 0.625, Some(1.0)));
/// Fable 5 rates; also the unknown Fable family fallback. 1h writes $20
/// (pricing page, read 2026-11-11).
const FABLE_TIER: ModelPrice = ModelPrice::new(10.0, 50.0, 1.0, 12.5).with_extended(20.0);
/// Fable 5.1 cache reads are $0.25 (official pricing, read 2026-10-08). 1h
/// writes $20 (read 2026-11-11).
const FABLE_5_1: ModelPrice = ModelPrice::new(10.0, 50.0, 0.25, 12.5).with_extended(20.0);
/// Fable 5.2 (released 2026-11-09; official pricing, read 2026-11-10): $8 in /
/// $40 out / $0.20 cache read / $10 cache write (5m); 1h writes $16 (read
/// 2026-11-11). There is no long-context tier. CHEAPER than Fable 5 / 5.1, so
/// it must NOT fall to the `claude-fable-` prefix fallback.
const FABLE_5_2: ModelPrice = ModelPrice::new(8.0, 40.0, 0.20, 10.0).with_extended(16.0);
/// Mythos 5.2 (limited availability): $8 in / $40 out / $0.20 cache read /
/// $10 cache write (5m) / $16 (1h),
/// https://platform.claude.com/docs/en/about-claude/pricing, read 2026-11-11.
/// Before this row the id fell to the claude group fallback (opus tier).
const MYTHOS_5_2: ModelPrice = ModelPrice::new(8.0, 40.0, 0.20, 10.0).with_extended(16.0);
/// Mythos 5.1 (limited availability): $10 in / $50 out / $0.25 cache read /
/// $12.50 cache write (5m) / $20 (1h), same page, read 2026-11-11.
const MYTHOS_5_1: ModelPrice = ModelPrice::new(10.0, 50.0, 0.25, 12.5).with_extended(20.0);
/// Mythos 5 (limited availability): $10 in / $50 out / $1 cache read /
/// $12.50 cache write (5m) / $20 (1h), same page, read 2026-11-11. Also the
/// unknown Mythos family fallback.
const MYTHOS_5: ModelPrice = ModelPrice::new(10.0, 50.0, 1.0, 12.5).with_extended(20.0);
/// gpt-5.5 / codex default {input 5.0, output 30.0, cache_read 0.5,
/// cache_creation 5.0}. The pricing page lists no cache-write rate for
/// gpt-5.5 (re-read 2026-11-11; later rows do list one), and the prompt-
/// caching guide says models before GPT-5.6 have "No additional cache-write
/// charge" (read 2026-11-11): a written token bills as ordinary input. Fresh
/// input excludes written tokens (`input - cached - written`), so the write
/// rate IS the input rate — 0 would make written tokens free. That holds for
/// a write at any TTL, so the row has no extended rate of its own (it falls
/// back to this input-rate write rate).
///
/// Long context ([`OPENAI_LONG`], the "gpt-5.5 (<272K context length)" row's
/// long twin, read 2026-11-11): $10 in / $1 cached / $45 out, writes at the
/// long input rate. Fast is 2.5x Standard: $12.50 / $1.25 / $75 (same page and
/// day). The page lists NO long-context Fast rate for gpt-5.5 (every cell
/// "-"), so a long Fast request prices at the long Standard rates — the
/// published number, not an invented one.
const GPT_5_5: ModelPrice = ModelPrice::new(5.0, 30.0, 0.5, 5.0)
    .with_long(OPENAI_LONG, tier(10.0, 45.0, 1.0, 10.0, None))
    .with_fast_standard_only(2.5);
/// gpt-5.5's Standard rates with no tier: the `gpt-5.5-` prefix
/// (`gpt-5.5-codex`, which the pricing page does not list) and the `group ==
/// "codex"` unknown-model fallback — neither may claim a tier nobody
/// verified for it.
const GPT_5_5_FLAT: ModelPrice = ModelPrice::new(5.0, 30.0, 0.5, 5.0);
/// gpt-5.6-sol (flagship, 2026-07-09 launch). developers.openai.com pricing,
/// read 2026-10-06: $4 in / $0.40 cached / $5 cache write / $20 out
/// ("promotional pricing … at least through November 21, 2026"; the page
/// read 2026-11-10 says "until November 30, 2026", post-promo price not
/// published). Corrected
/// from the launch-era $5/$30 row; OpenAI now lists a cache-write rate, so
/// the "codex has no cache-creation charge" convention no longer holds for
/// the rows re-read on 2026-10-06.
///
/// Every GPT-5.6 and later row carries the 12-hour cache-write rate, 2.5x its
/// input rate ("a 12-hour minimum lifetime at a write rate of 2.5× ordinary
/// input", developers.openai.com/api/docs/guides/prompt-caching, and the
/// "12h write" column of https://developers.openai.com/api/docs/pricing, both read 2026-11-11). Sol: $10.
///
/// Every GPT row below also carries its long-context tier ([`OPENAI_LONG`];
/// the "Long" rows of the same pricing page's Standard table, read
/// 2026-11-11: 2x input, cached input and both writes, 1.5x output) and its
/// Fast tier ([`OPENAI_FAST`], the "Fast" table: 2x, long context included).
/// Sol long: $8 / $0.80 / $10 / $20 / $30.
const GPT_5_6_SOL: ModelPrice = ModelPrice::new(4.0, 20.0, 0.40, 5.0)
    .with_extended(10.0)
    .with_long(OPENAI_LONG, tier(8.0, 30.0, 0.80, 10.0, Some(20.0)))
    .with_fast(OPENAI_FAST);
/// gpt-5.6-terra (mid tier). developers.openai.com/api/docs/models/gpt-5.6-terra,
/// read 2026-10-23: $2 in / $0.20 cached / $12 out. Corrected from the
/// launch-era (2026-07-10) $2.50 / $0.25 / $15 row. Cache write $2.50: the
/// 30m write column of https://developers.openai.com/api/docs/pricing and the
/// model page's "1.25x the uncached input token rate", both read 2026-11-11
/// (was 0). 12h writes $5 (same pricing page, read 2026-11-11). Long: $4 /
/// $0.40 / $5 / $10 / $18 (read 2026-11-11).
const GPT_5_6_TERRA: ModelPrice = ModelPrice::new(2.0, 12.0, 0.2, 2.5)
    .with_extended(5.0)
    .with_long(OPENAI_LONG, tier(4.0, 18.0, 0.40, 5.0, Some(10.0)))
    .with_fast(OPENAI_FAST);
/// gpt-5.6-luna (budget tier). developers.openai.com model page, read
/// 2026-10-06: $0.20 in / $0.02 cached / $1.20 out. Corrected from the
/// launch-era $1/$6. Cache write $0.25: the 30m write column of
/// https://developers.openai.com/api/docs/pricing and
/// https://developers.openai.com/api/docs/models/gpt-5.6-luna (1.25x input),
/// both read 2026-11-11 (was 0). 12h writes $0.50 (same page and day). Long:
/// $0.40 / $0.04 / $0.50 / $1 / $1.80 (read 2026-11-11).
const GPT_5_6_LUNA: ModelPrice = ModelPrice::new(0.20, 1.20, 0.02, 0.25)
    .with_extended(0.50)
    .with_long(OPENAI_LONG, tier(0.40, 1.80, 0.04, 0.50, Some(1.0)))
    .with_fast(OPENAI_FAST);
/// gpt-6-astra (generation-6 flagship, released 2026-09-03). OpenAI pricing
/// page, read 2026-10-06 (standard tier, ≤272k input): $10 in / $1 cached /
/// $12.50 cache write / $50 out; 12h writes $25 (read 2026-11-11). Long: $20
/// / $2 / $25 / $50 / $75 (read 2026-11-11). The Ultrafast tier is not
/// modeled (llmux records no such flag).
const GPT_6_ASTRA: ModelPrice = ModelPrice::new(10.0, 50.0, 1.0, 12.5)
    .with_extended(25.0)
    .with_long(OPENAI_LONG, tier(20.0, 75.0, 2.0, 25.0, Some(50.0)))
    .with_fast(OPENAI_FAST);
/// gpt-6.1-sol (released 2026-09-29; the openai/codex catalog's priority-1
/// default). OpenAI pricing page, read 2026-10-06 (standard, ≤272k input):
/// $2 in / $0.10 cached / $2.50 cache write / $10 out; 12h writes $5 (read
/// 2026-11-11). Long: $4 / $0.20 / $5 / $10 / $15 (read 2026-11-11).
const GPT_6_1_SOL: ModelPrice = ModelPrice::new(2.0, 10.0, 0.10, 2.5)
    .with_extended(5.0)
    .with_long(OPENAI_LONG, tier(4.0, 15.0, 0.20, 5.0, Some(10.0)))
    .with_fast(OPENAI_FAST);
/// gpt-6.1-terra (released 2026-10-26; generation 6.1's mid tier).
/// developers.openai.com model page and pricing page, read 2026-11-10
/// (standard, ≤272k input): $1 in / $0.05 cached / $1.25 cache write
/// (30-minute retention) / $5 out — half of 6.1-sol, so it must NOT fall to
/// the `gpt-6.1-` prefix branch. 12h writes $2.50 (pricing page, read
/// 2026-11-11). Long: $2 / $0.10 / $2.50 / $5 / $7.50 (read 2026-11-11).
const GPT_6_1_TERRA: ModelPrice = ModelPrice::new(1.0, 5.0, 0.05, 1.25)
    .with_extended(2.5)
    .with_long(OPENAI_LONG, tier(2.0, 7.50, 0.10, 2.5, Some(5.0)))
    .with_fast(OPENAI_FAST);
/// gpt-6-sol (released 2026-09-22). developers.openai.com model page, read
/// 2026-10-06: $2 in / $0.20 cached / $2.50 cache write / $10 out (absent
/// from the pricing page's flagship table then; the model page carries the
/// rates). 12h writes $5: the pricing page's flagship table, read 2026-11-11.
/// Long: $4 / $0.40 / $5 / $10 / $15 (read 2026-11-11).
const GPT_6_SOL: ModelPrice = ModelPrice::new(2.0, 10.0, 0.20, 2.5)
    .with_extended(5.0)
    .with_long(OPENAI_LONG, tier(4.0, 15.0, 0.40, 5.0, Some(10.0)))
    .with_fast(OPENAI_FAST);
/// gpt-6-luna (released 2026-09-22). OpenAI pricing page, read 2026-10-06
/// (standard, ≤272k input): $0.10 in / $0.01 cached / $0.125 cache write /
/// $0.50 out; 12h writes $0.25 (read 2026-11-11). Long: $0.20 / $0.02 /
/// $0.25 / $0.50 / $0.75 (read 2026-11-11).
const GPT_6_LUNA: ModelPrice = ModelPrice::new(0.10, 0.50, 0.01, 0.125)
    .with_extended(0.25)
    .with_long(OPENAI_LONG, tier(0.20, 0.75, 0.02, 0.25, Some(0.50)))
    .with_fast(OPENAI_FAST);
/// grok-4.5 (docs.x.ai/developers/models, read 2026-10-23): $2 in / $6 out /
/// $0.30 cached input below 200k prompt tokens (corrected from the $0.50
/// cached carried since 2026-07-14). xAI lists no cache-write price (re-read
/// 2026-11-11), so a written token bills as ordinary input: the write rate is
/// the input rate, never 0 (fresh input excludes written tokens). A prompt of
/// 200k tokens or more ([`XAI_LONG`]) bills the whole request at $4 / $12 /
/// $0.60 cached (writes at the long input rate; docs.x.ai/developers/pricing,
/// read 2026-11-11). Also the `group == "grok"` unknown-model fallback, tier
/// included. Like the codex rows, an
/// API-list-price EQUIVALENT for subscription traffic, not a billed amount
/// (docs/grok/spec.md §Compatibility).
const GROK_4_5: ModelPrice =
    ModelPrice::new(2.0, 6.0, 0.3, 2.0).with_long(XAI_LONG, tier(4.0, 12.0, 0.60, 4.0, None));
/// grok-4.6 (docs.x.ai, 2026-08-13): $2 in / $6 out; the $0.50/M cached input
/// was copied from the grok-4.5 row as it stood when this row landed, and is
/// LISTED on grok-4.6's own page (docs.x.ai, re-read 2026-09-23) — no longer
/// an inference. grok-4.5's own listed cached rate is $0.30 (read
/// 2026-10-23), so the two rows now differ. API-list-price equivalent for
/// subscription traffic, like the other grok rows. Write rate = input rate,
/// as for grok-4.5. Long ([`XAI_LONG`]): $4 / $12 / $1.00 cached
/// (docs.x.ai/developers/pricing, read 2026-11-11).
const GROK_4_6: ModelPrice =
    ModelPrice::new(2.0, 6.0, 0.5, 2.0).with_long(XAI_LONG, tier(4.0, 12.0, 1.0, 4.0, None));
/// grok-4.7 (docs.x.ai pricing, 2026-09-23): $2 in / $6 out / $0.50 cached
/// input, no separate cache-write price (write rate = input rate, as for
/// grok-4.5) — unchanged from grok-4.6, as is its long tier ([`XAI_LONG`]):
/// $4 / $12 / $1.00 cached (docs.x.ai/developers/pricing, read 2026-11-11).
/// API-list-price equivalent for subscription traffic.
const GROK_4_7: ModelPrice =
    ModelPrice::new(2.0, 6.0, 0.5, 2.0).with_long(XAI_LONG, tier(4.0, 12.0, 1.0, 4.0, None));
/// Free — all four rates zero. Applied to the CURATED OpenRouter set, every
/// member of which had `pricing.prompt == "0"` and `pricing.completion == "0"`
/// on the live `GET /api/v1/models` probe of 2026-08-21
/// (docs/openrouter/spec.md). Deliberately NOT the `group == "openrouter"`
/// fallback: OpenRouter also serves ~400 PAID models reachable through the
/// `or-<vendor>/<slug>` escape hatch, and asserting $0 for those would be an
/// invented number.
const OPENROUTER_FREE: ModelPrice = ModelPrice::new(0.0, 0.0, 0.0, 0.0);
/// Gemini 3.8 Flash (ai.google.dev/gemini-api/docs/pricing, updated
/// 2026-10-14): $0.75 in / $3.75 out / $0.075 cached input, no cache-write
/// rate. These rates apply "through December 31, 2026"; "starting January 1,
/// 2027" they are $1.50 / $7.50 / $0.15 ([`GEMINI_3_8_FLASH_2027`],
/// https://aistudio.google.com/docs/pricing, read 2026-11-11 — no time zone
/// named, so the change is at the UTC day boundary). One price for all three thinking variants
/// (`-high` / `-medium` / `-low` are one API model at different thinking
/// levels). An API-list-price-equivalent estimate for Antigravity
/// subscription traffic, not a billed amount. Also the `group == "gemini"`
/// unknown-model fallback, dated change included.
const GEMINI_3_8_FLASH: ModelPrice =
    ModelPrice::new(0.75, 3.75, 0.075, 0.0).with_changes(&[RateChange {
        from: RateDay::ymd(2027, 1, 1),
        price: GEMINI_3_8_FLASH_2027,
    }]);
/// Gemini 3.8 Flash from 2027-01-01 (see [`GEMINI_3_8_FLASH`]).
const GEMINI_3_8_FLASH_2027: ModelPrice = ModelPrice::new(1.50, 7.50, 0.15, 0.0);
/// Gemini 4 Argon (same page, updated 2026-10-14): $2 in / $10 out / $0.10
/// cached input, no cache-write rate — "through November 30, 2026". From
/// 2026-12-01 ("starting December 1, 2026") $4 in / $20 out / $0.20 cached
/// ([`GEMINI_4_ARGON_DEC_2026`], https://aistudio.google.com/docs/pricing,
/// re-read 2026-11-11; no time zone named, so the UTC day boundary). A
/// request is priced at the rates of its own UTC day.
/// API-list-price-equivalent estimate, like the Flash row.
const GEMINI_4_ARGON: ModelPrice =
    ModelPrice::new(2.0, 10.0, 0.10, 0.0).with_changes(&[RateChange {
        from: RateDay::ymd(2026, 12, 1),
        price: GEMINI_4_ARGON_DEC_2026,
    }]);
/// Gemini 4 Argon from 2026-12-01 (see [`GEMINI_4_ARGON`]).
const GEMINI_4_ARGON_DEC_2026: ModelPrice = ModelPrice::new(4.0, 20.0, 0.20, 0.0);

/// Look up the built-in default price for a *normalized*, lowercased model
/// slug. Exact matches first, then a sensible prefix fallback (so e.g.
/// `claude-opus-4-8-20260101` still resolves to the opus tier). Returns `None`
/// when nothing matches — callers apply the group fallback.
fn builtin_price(model_norm_lower: &str) -> Option<ModelPrice> {
    // Exact (post-normalization) matches.
    let exact = match model_norm_lower {
        "claude-opus-5-5" => Some(OPUS_5_5),
        // `claude-opus-4-0` is Anthropic's alias of the Opus 4 snapshot.
        "claude-opus-4-1" | "claude-opus-4" | "claude-opus-4-0" => Some(OPUS_4_1),
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6"
        | "claude-opus-4-5" => Some(OPUS_TIER),
        "claude-sonnet-5-5" => Some(SONNET_5_5),
        "claude-sonnet-5" => Some(SONNET_5),
        "claude-sonnet-4-6" | "claude-sonnet-4-5" => Some(SONNET_TIER),
        "claude-haiku-5-5" => Some(HAIKU_5_5),
        // Records from before the 2026-10-23 float (accounting never floats).
        "claude-haiku-4-5" => Some(HAIKU_TIER),
        "claude-fable-5-2" => Some(FABLE_5_2),
        "claude-fable-5-1" => Some(FABLE_5_1),
        "claude-fable-5" => Some(FABLE_TIER),
        "claude-mythos-5-2" => Some(MYTHOS_5_2),
        "claude-mythos-5-1" => Some(MYTHOS_5_1),
        "claude-mythos-5" => Some(MYTHOS_5),
        "gpt-5.5" => Some(GPT_5_5),
        "gpt-5.6" | "gpt-5.6-sol" => Some(GPT_5_6_SOL),
        "gpt-5.6-terra" => Some(GPT_5_6_TERRA),
        "gpt-5.6-luna" => Some(GPT_5_6_LUNA),
        "gpt-6" | "gpt-6-astra" => Some(GPT_6_ASTRA),
        "gpt-6-sol" => Some(GPT_6_SOL),
        "gpt-6-luna" => Some(GPT_6_LUNA),
        "gpt-6.1" | "gpt-6.1-sol" => Some(GPT_6_1_SOL),
        "gpt-6.1-terra" => Some(GPT_6_1_TERRA),
        "grok-4.5" => Some(GROK_4_5),
        "grok-4.7" => Some(GROK_4_7),
        "grok-4.6" => Some(GROK_4_6),
        // `[1m]` ids arrive here already stripped by `normalize_model`. The
        // bare family aliases are listed too: unlike the claude aliases they
        // are not resolved by `normalize_model`, and without its own row an
        // `argon` booked under the client's name would fall to the group
        // fallback and be priced as Flash.
        "gemini"
        | "gemini-3.8-flash"
        | "gemini-3.8-flash-high"
        | "gemini-3.8-flash-medium"
        | "gemini-3.8-flash-low" => Some(GEMINI_3_8_FLASH),
        "argon" | "gemini-4-argon" => Some(GEMINI_4_ARGON),
        _ => None,
    };
    if exact.is_some() {
        return exact;
    }
    // Curated OpenRouter free models, keyed by the UPSTREAM slug — which is
    // what the activity log records (the proxy rewrites `or-ox-alpha` to
    // `stealth/ox-alpha` before the request leaves, and `finished_meta`
    // reports the wire model). `crate::catalog::OPENROUTER_MODELS` is the SSOT
    // for the set, so adding a row there prices it automatically.
    if crate::catalog::OPENROUTER_MODELS
        .iter()
        .any(|(_, slug, ..)| *slug == model_norm_lower)
    {
        return Some(OPENROUTER_FREE);
    }
    // Prefix fallback for versioned / suffixed slugs.
    if model_norm_lower.starts_with("claude-opus-5-5-") {
        // Opus 5.5 is CHEAPER than the opus tier ($4/$20 vs $5/$25), so a dated
        // snapshot must be caught here before the generic `claude-opus-` branch
        // below overcharges it — same ordering as `gpt-6-astra-` ahead of
        // `gpt-6-`. The trailing `-` is the version boundary: `claude-opus-5-50-*`
        // and `claude-opus-5-5x` are DIFFERENT models and must miss this branch
        // and fall to the opus tier.
        Some(OPUS_5_5)
    } else if model_norm_lower.starts_with("claude-opus-4-1-")
        || model_norm_lower.starts_with("claude-opus-4-0-")
        || is_dated_snapshot(model_norm_lower, "claude-opus-4-")
    {
        // Opus 4.1 / 4 snapshots (`claude-opus-4-1-20250805`,
        // `claude-opus-4-20250514`) cost three times the Opus tier. Only a
        // date after `claude-opus-4-` is Opus 4: `claude-opus-4-5-*` and the
        // other 4.x generations must miss this branch.
        Some(OPUS_4_1)
    } else if model_norm_lower.starts_with("claude-opus-") {
        Some(OPUS_TIER)
    } else if model_norm_lower.starts_with("claude-sonnet-5-5-") {
        Some(SONNET_5_5)
    } else if model_norm_lower.starts_with("claude-sonnet-5-") {
        // Sonnet 5 / 5.5 are CHEAPER than the sonnet tier — same ordering
        // argument as `claude-opus-5-5-` above. The trailing `-` is the
        // version boundary (`claude-sonnet-50-*` must miss this branch).
        Some(SONNET_5)
    } else if model_norm_lower.starts_with("claude-sonnet-") {
        Some(SONNET_TIER)
    } else if model_norm_lower.starts_with("claude-haiku-5-5-") {
        Some(HAIKU_5_5)
    } else if model_norm_lower.starts_with("claude-haiku-") {
        Some(HAIKU_TIER)
    } else if model_norm_lower.starts_with("claude-fable-5-2-") {
        // Fable 5.2 is CHEAPER than the Fable tier, so a dated snapshot must
        // be caught before `claude-fable-` overcharges it. The trailing `-` is
        // the version boundary: `claude-fable-5-20-*` must miss this branch.
        Some(FABLE_5_2)
    } else if model_norm_lower.starts_with("claude-fable-5-1-") {
        Some(FABLE_5_1)
    } else if model_norm_lower.starts_with("claude-fable-") {
        Some(FABLE_TIER)
    } else if model_norm_lower.starts_with("claude-mythos-5-2-") {
        // Same version boundary and ordering as the Fable rows above.
        Some(MYTHOS_5_2)
    } else if model_norm_lower.starts_with("claude-mythos-5-1-") {
        Some(MYTHOS_5_1)
    } else if model_norm_lower.starts_with("claude-mythos-") {
        Some(MYTHOS_5)
    } else if model_norm_lower.starts_with("gpt-5.5-") {
        // Generation boundary: bare `gpt-5.5` matched exactly above; the
        // prefix branch requires the `-` so `gpt-5.50-*` never takes 5.5
        // rates (mirrors codex.rs `supports_extended_efforts`). Flat: the
        // pricing page lists no `gpt-5.5-*` id, so no tier is claimed.
        Some(GPT_5_5_FLAT)
    } else if model_norm_lower.starts_with("gpt-5.6-terra-") {
        Some(GPT_5_6_TERRA)
    } else if model_norm_lower.starts_with("gpt-5.6-luna-") {
        Some(GPT_5_6_LUNA)
    } else if model_norm_lower.starts_with("gpt-5.6-") {
        // Sol is the flagship default: `gpt-5.6-sol` (exact, above) and any
        // future dated `gpt-5.6-sol-*` snapshot resolve here. Same generation
        // boundary: `gpt-5.60-*` must NOT take 5.6 rates.
        Some(GPT_5_6_SOL)
    } else if model_norm_lower.starts_with("gpt-6.1-terra-") {
        // Terra is half sol's price, so a dated / suffixed terra id must be
        // caught before the `gpt-6.1-` branch below overcharges it.
        Some(GPT_6_1_TERRA)
    } else if model_norm_lower.starts_with("gpt-6.1-") {
        // Sol is generation 6.1's flagship, so it is the `gpt-6.1-` default
        // (terra has its own exact and prefix rows above). `gpt-6.10-*` must
        // miss this branch, hence the required `-`.
        Some(GPT_6_1_SOL)
    } else if model_norm_lower.starts_with("gpt-6-astra-") {
        Some(GPT_6_ASTRA)
    } else if model_norm_lower.starts_with("gpt-6-sol-") {
        Some(GPT_6_SOL)
    } else if model_norm_lower.starts_with("gpt-6-luna-") {
        Some(GPT_6_LUNA)
    } else if model_norm_lower.starts_with("gemini-4-argon-") {
        // Argon is PRICIER than the gemini group fallback (Flash), so a
        // dated / suffixed Argon id must be caught here rather than
        // underpriced. The trailing `-` is the version boundary
        // (`gemini-4-argonx` must miss this branch).
        Some(GEMINI_4_ARGON)
    } else if model_norm_lower.starts_with("gpt-6-") {
        // Astra is generation 6's flagship, so it is the `gpt-6-` default
        // exactly as sol is for `gpt-5.6-` (sol/luna have their own exact and
        // prefix rows above). The required `-`
        // is the generation boundary: `gpt-60-*` and `gpt-6.5-*` are DIFFERENT
        // generations and must miss this branch (mirrors codex.rs
        // `supports_extended_efforts`).
        Some(GPT_6_ASTRA)
    } else {
        None
    }
}

/// Whether `model` is `prefix` followed by exactly an 8-digit date
/// (`claude-opus-4-20250514`).
fn is_dated_snapshot(model: &str, prefix: &str) -> bool {
    model
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.len() == 8 && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Resolve the price for `(group, model)`, honoring config `overrides` first.
///
/// Resolution order:
/// 1. `overrides` keyed by the **normalized** model (case preserved as written
///    in config, but matched case-insensitively against the normalized model).
/// 2. The built-in default table (exact then prefix), case-insensitive.
/// 3. Group fallback: `group == "claude"` → opus-tier rates; `group ==
///    "codex"` → gpt-5.5's Standard rates (no tier); `"grok"` → grok-4.5
///    rates (long tier included); `"gemini"` → Gemini 3.8 Flash rates (dated
///    change included); any other group → `None` (all-zero cost).
///
/// `overrides` keys are normalized + lowercased on read, so a config can use
/// either the display slug (`claude-opus-4-8[1m]`) or the bare slug.
pub fn price_for(
    group: &str,
    model: &str,
    overrides: &HashMap<String, ModelPrice>,
) -> Option<ModelPrice> {
    let norm = normalize_model(model);
    let norm_lower = norm.to_ascii_lowercase();

    // 1. Config override wins. Match case-insensitively on the normalized slug.
    if !overrides.is_empty() {
        if let Some(p) = overrides.get(&norm) {
            return Some(*p);
        }
        for (k, v) in overrides {
            if normalize_model(k).eq_ignore_ascii_case(&norm) {
                return Some(*v);
            }
        }
    }

    // 2. Built-in default table.
    if let Some(p) = builtin_price(&norm_lower) {
        return Some(p);
    }

    // 3. Group fallback. `openrouter` deliberately has NONE: the group spans
    // free and paid models from ~60 vendors, so there is no defensible
    // representative rate — an uncurated openrouter model is priced `None`
    // (unknown), which `cost_usd` renders as 0.0 exactly as it does for any
    // other unknown group.
    match group.to_ascii_lowercase().as_str() {
        "claude" => Some(OPUS_TIER),
        "codex" => Some(GPT_5_5_FLAT),
        "grok" => Some(GROK_4_5),
        "gemini" => Some(GEMINI_3_8_FLASH),
        _ => None,
    }
}

/// API-equivalent USD cost of `tokens` in the STANDARD class under
/// `(group, model)`'s price. Unknown / zero-rate model → `0.0`. Never panics.
/// A request or aggregate with a non-standard class must go through
/// [`class_cost`] / [`request_cost_usd`] / [`aggregate_cost`]; this is what
/// every record without class facts (history from before classes existed)
/// prices at. See [`priced_cost`].
pub fn cost_usd(
    group: &str,
    model: &str,
    tokens: &TokenCounts,
    overrides: &HashMap<String, ModelPrice>,
) -> f64 {
    priced_cost(group, model, tokens, overrides).unwrap_or(0.0)
}

/// API-equivalent USD cost of ONE finished request: its top-level `tokens`
/// in its `class` at `(group, model)` rates plus each sub-usage (#278:
/// advisor, compaction, billed fallback attempt) at its own model's rates
/// (`None` → `model`) in its own class. The figure every per-request view
/// shows, so per-request costs add up to the per-model aggregates that book
/// the same sub-usages under their models.
pub fn request_cost_usd(
    group: &str,
    model: &str,
    class: PriceClass,
    tokens: &TokenCounts,
    sub_usage: &[crate::tui::SubUsage],
    overrides: &HashMap<String, ModelPrice>,
) -> f64 {
    sub_usage.iter().fold(
        class_cost(group, model, class, tokens, overrides).unwrap_or(0.0),
        |sum, sub| {
            sum + class_cost(
                group,
                sub.model.as_deref().unwrap_or(model),
                PriceClass::STANDARD,
                &sub.tokens,
                overrides,
            )
            .unwrap_or(0.0)
        },
    )
}

/// [`cost_usd`] without the `0.0` sentinel: `None` means "no rate known for
/// this `(group, model)`", so a caller can never mistake a missing rate for a
/// free request (usage-stats review — the `priced` flag and the cost must
/// come from ONE lookup, not two calls that merely agree today). The
/// standard class; see [`class_cost`].
pub fn priced_cost(
    group: &str,
    model: &str,
    tokens: &TokenCounts,
    overrides: &HashMap<String, ModelPrice>,
) -> Option<f64> {
    class_cost(group, model, PriceClass::STANDARD, tokens, overrides)
}

/// API-equivalent USD cost of `tokens`, all of one price `class`, under
/// `(group, model)`'s price; `None` = no rate known.
///
/// `cost = input·in + output·out + cache_read·cr + (cache_creation −
/// extended)·cc + extended·cc_ext` (rates per 1e6 tokens, the ones `class`
/// selects: [`ModelPrice::rates`]) `+ web searches ·` [`web_search_fee_usd`];
/// absent (`None`) counters contribute `0`, and the extended subset is
/// clamped to `cache_creation`. An unbilled class costs `0.0`. Without a
/// split or searches the two extra terms are `+ 0.0`, so a standard request
/// prices bit for bit as before #278.
pub fn class_cost(
    group: &str,
    model: &str,
    class: PriceClass,
    tokens: &TokenCounts,
    overrides: &HashMap<String, ModelPrice>,
) -> Option<f64> {
    let price = price_for(group, model, overrides)?;
    Some(price_tokens(&price, group, class, tokens))
}

/// The one cost formula every entry point reduces to.
fn price_tokens(price: &ModelPrice, group: &str, class: PriceClass, tokens: &TokenCounts) -> f64 {
    if class.unbilled {
        return 0.0;
    }
    let rates = price.rates(class);
    let per_m = |count: u64, rate: f64| (count as f64) * rate / 1_000_000.0;
    let written = tokens.cache_creation.unwrap_or(0);
    let extended = tokens.cache_creation_extended.unwrap_or(0).min(written);
    let searches = tokens.web_search_requests.unwrap_or(0) as f64;
    per_m(tokens.input, rates.input)
        + per_m(tokens.output, rates.output)
        + per_m(tokens.cache_read.unwrap_or(0), rates.cache_read)
        + per_m(written - extended, rates.cache_creation)
        + per_m(extended, rates.extended_write_rate())
        + searches * web_search_fee_usd(group)
}

/// API-equivalent USD cost of an AGGREGATE of requests: `total` is every
/// request's (and sub-usage's) counters summed, `split` the part of it in a
/// non-standard class, per class. The standard remainder (`total` minus the
/// split) prices at the standard rates and each class at its own, so the
/// result is the sum of the per-request costs. `None` = no rate known. An
/// empty split prices `total` exactly as [`priced_cost`] does.
pub fn aggregate_cost(
    group: &str,
    model: &str,
    total: &TokenCounts,
    split: &ClassSplit,
    overrides: &HashMap<String, ModelPrice>,
) -> Option<f64> {
    let price = price_for(group, model, overrides)?;
    let standard = split.standard_part(total);
    Some(split.0.iter().fold(
        price_tokens(&price, group, PriceClass::STANDARD, &standard),
        |sum, (class, tokens)| sum + price_tokens(&price, group, *class, tokens),
    ))
}

/// [`aggregate_cost`] with the `0.0` sentinel for an unknown rate.
pub fn aggregate_cost_usd(
    group: &str,
    model: &str,
    total: &TokenCounts,
    split: &ClassSplit,
    overrides: &HashMap<String, ModelPrice>,
) -> f64 {
    aggregate_cost(group, model, total, split, overrides).unwrap_or(0.0)
}

/// The NON-STANDARD part of an aggregate's counters, per price class: the
/// tokens of every request (and sub-usage) folded into the aggregate whose
/// class is not the standard one. The standard part is the aggregate's total
/// minus all of these, so an aggregate keeps its existing total counters and
/// carries only this, which is empty for every aggregate of standard
/// requests (all history from before classes existed).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClassSplit(std::collections::BTreeMap<PriceClass, TokenCounts>);

impl ClassSplit {
    /// Fold one request's (or sub-usage's) `tokens` of `class` in. A
    /// standard class adds nothing (it lives in the total).
    pub fn add(&mut self, class: PriceClass, tokens: &TokenCounts) {
        if class.is_standard() {
            return;
        }
        let entry = self.0.entry(class).or_default();
        *entry = entry.saturating_add(*tokens);
    }

    /// Merge another aggregate's split (history hydration).
    pub fn merge(&mut self, other: &ClassSplit) {
        for (class, tokens) in &other.0 {
            self.add(*class, tokens);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The non-standard classes and their counters, in class order.
    pub fn iter(&self) -> impl Iterator<Item = (&PriceClass, &TokenCounts)> {
        self.0.iter()
    }

    /// `total` minus every class here: the standard-class remainder.
    pub fn standard_part(&self, total: &TokenCounts) -> TokenCounts {
        self.0
            .values()
            .fold(*total, |rest, tokens| rest.saturating_sub(*tokens))
    }

    /// The split as document shares, each unpriced (`cost_usd` 0) — see
    /// [`price_shares`].
    pub fn shares(&self) -> Vec<ClassShare> {
        self.0
            .iter()
            .map(|(class, tokens)| ClassShare {
                class: *class,
                tokens: *tokens,
                cost_usd: 0.0,
            })
            .collect()
    }

    /// Rebuild a split from document shares.
    pub fn from_shares(shares: &[ClassShare]) -> Self {
        let mut split = Self::default();
        for share in shares {
            split.add(share.class, &share.tokens);
        }
        split
    }
}

/// One non-standard price class's part of an aggregate, as the dashboard
/// document carries it (additive `price_classes` on model rows, usage rows and
/// tenant cells): the class, its counters, and their cost at the class's
/// rates, so a client can tell the standard remainder's cost apart.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClassShare {
    pub class: PriceClass,
    pub tokens: TokenCounts,
    #[serde(default)]
    pub cost_usd: f64,
}

/// Price an aggregate given as `total` + document `shares`: fills each
/// share's `cost_usd` and returns the aggregate's cost ([`aggregate_cost`]);
/// `None` = no rate known (every share stays 0).
pub fn price_shares(
    group: &str,
    model: &str,
    total: &TokenCounts,
    shares: &mut [ClassShare],
    overrides: &HashMap<String, ModelPrice>,
) -> Option<f64> {
    let price = price_for(group, model, overrides)?;
    for share in shares.iter_mut() {
        share.cost_usd = price_tokens(&price, group, share.class, &share.tokens);
    }
    aggregate_cost(
        group,
        model,
        total,
        &ClassSplit::from_shares(shares),
        overrides,
    )
}

/// Anthropic's server web-search fee: "$10 USD per 1,000 searches, plus
/// standard token costs" (https://platform.claude.com/docs/en/about-claude/pricing,
/// "Web search tool", read 2026-11-11). Web fetch is "no additional cost"
/// on the same page, so it is not counted.
pub const ANTHROPIC_WEB_SEARCH_USD: f64 = 10.0 / 1_000.0;

/// The per-search fee of `group`'s server web search, USD: Anthropic's for
/// the claude group, `0.0` elsewhere. A provider fee, not a token rate, so a
/// config `pricing` override (token rates) does not change it. Only Anthropic
/// reports a per-request count llmux reads (`server_tool_use`); xAI publishes
/// per-call prices but its per-tool usage counts are not documented, and the
/// grok translate path refuses server tools.
pub fn web_search_fee_usd(group: &str) -> f64 {
    if group.eq_ignore_ascii_case("claude") {
        ANTHROPIC_WEB_SEARCH_USD
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn october_2026_claude_reference_rates_and_override_precedence() {
        // Official pricing, checked 2026-10-08: Haiku <=100k prompt / 5m writes
        // (1h writes $0.20, read 2026-11-11); the >100,000 tier rides on the
        // same row (see the long-context tests).
        let haiku = HAIKU_5_5;
        assert_eq!(
            haiku.standard(),
            ModelPrice::new(0.10, 0.50, 0.01, 0.125)
                .with_extended(0.20)
                .standard()
        );
        for model in [
            "haiku",
            "haiku[1m]",
            "haiku-5-5",
            "claude-haiku-5-5",
            "claude-haiku-5-5[1m]",
            "claude-haiku-5-5-20261007",
        ] {
            assert_eq!(price_for("claude", model, &empty()), Some(haiku), "{model}");
        }
        // A record carrying a Haiku 4.5 id is history Haiku 4.5 served (new
        // traffic is recorded as `claude-haiku-5-5` since 2026-10-23), so it
        // keeps 4.5 rates — the routing float must not reprice it.
        for model in [
            "claude-haiku-4-5",
            "claude-haiku-4-5-20251001",
            "claude-haiku-4-5[1m]",
        ] {
            assert_eq!(
                price_for("claude", model, &empty()),
                Some(HAIKU_TIER),
                "{model}"
            );
        }
        // An override keyed by the 4.5 id prices only 4.5-id records.
        let custom = ModelPrice::new(9.0, 8.0, 7.0, 6.0);
        let overrides = HashMap::from([("claude-haiku-4-5".to_string(), custom)]);
        assert_eq!(
            price_for("claude", "claude-haiku-4-5", &overrides),
            Some(custom)
        );
        for model in ["claude-haiku-5-5", "haiku"] {
            assert_eq!(
                price_for("claude", model, &overrides),
                Some(haiku),
                "{model}"
            );
        }
        // HAIKU_TIER is also the unknown-haiku fallback.
        assert_eq!(
            builtin_price("claude-haiku-5-50-20261007"),
            Some(HAIKU_TIER)
        );
        assert_eq!(builtin_price("claude-haiku-5-5x"), Some(HAIKU_TIER));
        for (model, expected) in [
            ("sonnet", 0.10),
            ("claude-sonnet-5-5-20260928", 0.10),
            ("claude-sonnet-5", 0.20),
            ("claude-sonnet-5-20260701", 0.20),
            ("claude-sonnet-5-50-20261007", 0.20),
            // `fable` floats to Fable 5.2 since 2026-11-10 ($0.20 cache
            // read); the version alias and the 5.1 ids keep 5.1's $0.25.
            ("fable", 0.20),
            ("fable-5-2", 0.20),
            ("fable-5-1", 0.25),
            ("claude-fable-5-1", 0.25),
            ("claude-fable-5-1-20260901", 0.25),
            ("claude-fable-5", 1.0),
            ("claude-fable-5-10-20261007", 1.0),
        ] {
            approx(
                cost_usd("claude", model, &tc(0, 0, Some(1_000_000), None), &empty()),
                expected,
            );
        }
        let custom = ModelPrice::new(9.0, 8.0, 7.0, 6.0);
        let overrides = HashMap::from([("CLAUDE-HAIKU-5-5[1m]".to_string(), custom)]);
        assert_eq!(price_for("claude", "haiku", &overrides), Some(custom));
        // A long-context tier is a fact of ONE request (its price class),
        // never inferred by comparing a summed total to a request threshold:
        // 1M input tokens priced without a class are standard.
        approx(
            cost_usd("claude", "haiku", &tc(1_000_000, 0, None, None), &empty()),
            0.10,
        );
    }

    // ---- openrouter pricing (docs/openrouter/spec.md §R6) ----
    #[test]
    fn openrouter_curated_free_models_price_at_zero() {
        let overrides = std::collections::HashMap::new();
        // Priced by the UPSTREAM slug — the activity log records what went on
        // the wire, not the `or-…` id the client typed.
        for (_, slug, ..) in crate::catalog::OPENROUTER_MODELS {
            let p = price_for("openrouter", slug, &overrides)
                .unwrap_or_else(|| panic!("{slug} priced"));
            assert_eq!(
                (p.input, p.output, p.cache_read, p.cache_creation),
                (0.0, 0.0, 0.0, 0.0),
                "{slug} is a free model"
            );
        }
    }

    #[test]
    fn openrouter_group_has_no_fallback_price() {
        let overrides = std::collections::HashMap::new();
        // The group fronts ~400 PAID models through the `or-<vendor>/<slug>`
        // escape hatch, so an uncurated slug must be UNKNOWN rather than
        // silently claimed free (which would understate real spend).
        assert!(
            price_for("openrouter", "anthropic/claude-sonnet-4", &overrides).is_none(),
            "an uncurated openrouter model must not inherit a group price"
        );
        // …and a config override still wins, so a user CAN price one.
        let mut overrides = std::collections::HashMap::new();
        overrides.insert(
            "anthropic/claude-sonnet-4".to_string(),
            ModelPrice::new(3.0, 15.0, 0.3, 3.75),
        );
        let p = price_for("openrouter", "anthropic/claude-sonnet-4", &overrides)
            .expect("override applies");
        assert_eq!((p.input, p.output), (3.0, 15.0));
    }

    // ---- C14: grok pricing ----
    #[test]
    fn c14_grok_prices_and_group_fallback() {
        let overrides = std::collections::HashMap::new();
        // grok-4.5 (docs.x.ai models, read 2026-10-23): $0.30 cached below
        // 200k, not the $0.50 that grok-4.6 / grok-4.7 list. No separate
        // cache-write price: writes bill at the input rate (2026-11-11).
        let p = price_for("grok", "grok-4.5", &overrides).expect("grok-4.5 priced");
        assert_eq!(
            (p.input, p.output, p.cache_read, p.cache_creation),
            (2.0, 6.0, 0.3, 2.0)
        );
        let p6 = price_for("grok", "grok-4.6", &overrides).expect("grok-4.6 priced");
        assert_eq!(
            (p6.input, p6.output, p6.cache_read, p6.cache_creation),
            (2.0, 6.0, 0.5, 2.0)
        );
        // grok-4.7 (docs.x.ai 2026-09-23): same rates as 4.6 (its ≥200k
        // long-context tier is pinned by the price-class tests).
        let p7 = price_for("grok", "grok-4.7", &overrides).expect("grok-4.7 priced");
        assert_eq!(
            (p7.input, p7.output, p7.cache_read, p7.cache_creation),
            (2.0, 6.0, 0.5, 2.0)
        );
        let f = price_for("grok", "grok-build-0.1", &overrides).expect("group fallback");
        assert_eq!(
            (f.input, f.output),
            (2.0, 6.0),
            "unknown grok model → grok fallback"
        );
    }

    // ---- G16: gemini pricing (docs/gemini/spec.md §R7) ----
    #[test]
    fn g16_gemini_flash_variants_and_1m_ids_price_at_flash() {
        let flash = (0.75, 3.75, 0.075, 0.0);
        for model in [
            "gemini-3.8-flash-high",
            "gemini-3.8-flash-high[1m]",
            "gemini-3.8-flash-medium",
            "gemini-3.8-flash-medium[1m]",
            "gemini-3.8-flash-low",
            "gemini-3.8-flash-low[1m]",
            "GEMINI-3.8-FLASH-HIGH",
            "gemini",
            "gemini[1m]",
            "gemini-3.8-flash",
        ] {
            let p = price_for("gemini", model, &empty()).unwrap_or_else(|| panic!("{model}"));
            assert_eq!(
                (p.input, p.output, p.cache_read, p.cache_creation),
                flash,
                "{model}"
            );
            // A model-id match, not the group fallback: the rate holds even
            // when the group is unknown.
            assert_eq!(
                builtin_price(&normalize_model(model).to_ascii_lowercase()),
                Some(GEMINI_3_8_FLASH),
                "{model}"
            );
        }
    }

    #[test]
    fn g16_gemini_argon_ids_and_alias_price_at_argon() {
        let argon = (2.0, 10.0, 0.10, 0.0);
        for model in [
            "gemini-4-argon",
            "gemini-4-argon[1m]",
            "argon",
            "argon[1m]",
            "gemini-4-argon-20261014",
        ] {
            let p = price_for("gemini", model, &empty()).unwrap_or_else(|| panic!("{model}"));
            assert_eq!(
                (p.input, p.output, p.cache_read, p.cache_creation),
                argon,
                "{model}"
            );
        }
        // Version boundary: a different model id must not take Argon rates.
        assert_eq!(builtin_price("gemini-4-argonx"), None);
    }

    #[test]
    fn g16_unknown_gemini_model_falls_back_to_flash() {
        let p = price_for("gemini", "gemini-9-mystery", &empty()).expect("group fallback");
        assert_eq!(p, GEMINI_3_8_FLASH);
        // One million output tokens at the Flash list rate.
        approx(
            cost_usd(
                "gemini",
                "gemini-9-mystery",
                &tc(0, 1_000_000, None, None),
                &empty(),
            ),
            3.75,
        );
        // The fallback is group-scoped: an unknown model in an unknown group
        // stays unpriced.
        assert!(price_for("weirdgroup", "gemini-9-mystery", &empty()).is_none());
    }

    #[test]
    fn g16_every_curated_gemini_row_is_priced_by_id() {
        for &(id, ..) in crate::catalog::GEMINI_MODELS {
            assert!(
                builtin_price(&normalize_model(id).to_ascii_lowercase()).is_some(),
                "catalog row {id} has no model-id price"
            );
        }
    }

    const EPS: f64 = 1e-9;

    fn empty() -> HashMap<String, ModelPrice> {
        HashMap::new()
    }

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < EPS, "expected ~{b}, got {a}");
    }

    fn tc(input: u64, output: u64, cr: Option<u64>, cc: Option<u64>) -> TokenCounts {
        TokenCounts {
            input,
            output,
            cache_read: cr,
            cache_creation: cc,
            cache_creation_extended: None,
            web_search_requests: None,
        }
    }

    #[test]
    fn opus_input_one_million_is_five_dollars() {
        let cost = cost_usd(
            "claude",
            "claude-opus-4-8",
            &tc(1_000_000, 0, None, None),
            &empty(),
        );
        approx(cost, 5.00);
    }

    #[test]
    fn opus_cache_read_one_million_is_fifty_cents() {
        let cost = cost_usd(
            "claude",
            "claude-opus-4-8",
            &tc(0, 0, Some(1_000_000), None),
            &empty(),
        );
        approx(cost, 0.50);
    }

    #[test]
    fn opus_cache_creation_one_million_is_six_twentyfive() {
        let cost = cost_usd(
            "claude",
            "claude-opus-4-8",
            &tc(0, 0, None, Some(1_000_000)),
            &empty(),
        );
        approx(cost, 6.25);
    }

    #[test]
    fn opus_5_5_input_one_million_is_four_dollars() {
        let cost = cost_usd(
            "claude",
            "claude-opus-5-5",
            &tc(1_000_000, 0, None, None),
            &empty(),
        );
        approx(cost, 4.00);
    }

    #[test]
    fn opus_5_5_output_one_million_is_twenty_dollars() {
        let cost = cost_usd(
            "claude",
            "claude-opus-5-5",
            &tc(0, 1_000_000, None, None),
            &empty(),
        );
        approx(cost, 20.00);
    }

    #[test]
    fn opus_5_5_cache_read_one_million_is_twenty_cents() {
        let cost = cost_usd(
            "claude",
            "claude-opus-5-5",
            &tc(0, 0, Some(1_000_000), None),
            &empty(),
        );
        approx(cost, 0.20);
    }

    #[test]
    fn opus_5_5_cache_creation_one_million_is_five_dollars() {
        let cost = cost_usd(
            "claude",
            "claude-opus-5-5",
            &tc(0, 0, None, Some(1_000_000)),
            &empty(),
        );
        approx(cost, 5.00);
    }

    /// The display slug carries the client-side `[1m]` denominator, not a
    /// different upstream model — `normalize_model` strips it, so both spellings
    /// must price identically.
    #[test]
    fn opus_5_5_display_slug_prices_like_the_bare_slug() {
        let cost = cost_usd(
            "claude",
            "claude-opus-5-5[1m]",
            &tc(1_000_000, 0, None, None),
            &empty(),
        );
        approx(cost, 4.00);
    }

    /// `opus` floated onto Opus 5.5 on 2026-09-23 while `opus-5` stayed pinned
    /// to Opus 5. Pricing resolves through `normalize_model`, so the two aliases
    /// must now land on DIFFERENT rates — this is the guard against the alias
    /// roll silently leaving 5.5 traffic billed at the old opus tier.
    #[test]
    fn opus_alias_prices_at_five_five_while_opus_5_keeps_the_opus_tier() {
        approx(
            cost_usd("claude", "opus", &tc(1_000_000, 0, None, None), &empty()),
            4.00,
        );
        approx(
            cost_usd("claude", "opus-5", &tc(1_000_000, 0, None, None), &empty()),
            5.00,
        );
    }

    /// A dated Opus 5.5 snapshot takes the 5.5 rate, not the opus tier — the
    /// `claude-opus-5-5-` branch has to sit AHEAD of the generic `claude-opus-`
    /// fallback or every snapshot is billed 25% high. The version boundary is
    /// the trailing `-`: `claude-opus-5-50-*` / `claude-opus-5-5x` are other
    /// models and stay on the opus tier, as does a bare `claude-opus-5-` stem.
    #[test]
    fn opus_5_5_dated_snapshot_takes_the_five_five_rate_not_the_opus_tier() {
        approx(
            cost_usd(
                "claude",
                "claude-opus-5-5-20260922",
                &tc(1_000_000, 0, None, None),
                &empty(),
            ),
            4.00,
        );
        for near_miss in [
            "claude-opus-5-",
            "claude-opus-5-50-20260922",
            "claude-opus-5-5x",
        ] {
            approx(
                cost_usd("claude", near_miss, &tc(1_000_000, 0, None, None), &empty()),
                5.00,
            );
        }
    }

    #[test]
    fn gpt_5_5_output_one_million_is_thirty_dollars() {
        let cost = cost_usd("codex", "gpt-5.5", &tc(0, 1_000_000, None, None), &empty());
        approx(cost, 30.00);
    }

    #[test]
    fn cache_writes_bill_as_input_on_rows_without_a_separate_write_rate() {
        // Fresh input excludes cache writes (input - cached - written), so a
        // row with no separate write rate must price the written tokens at
        // its input rate, never $0. OpenAI: models before GPT-5.6 have "No
        // additional cache-write charge" — written tokens bill as ordinary
        // input (prompt-caching guide, read 2026-11-11). xAI lists no
        // cache-write price either (docs.x.ai/developers/models, same day).
        // Usage 12000 in / 4000 cached / 7000 written, as booked:
        let tokens = tc(1_000, 0, Some(4_000), Some(7_000));
        for (group, model, input, cached) in [
            ("codex", "gpt-5.5", 5.0, 0.5),
            ("codex", "gpt-5.5-codex", 5.0, 0.5),
            ("codex", "some-future-codex-model", 5.0, 0.5),
            ("grok", "grok-4.5", 2.0, 0.3),
            ("grok", "grok-4.6", 2.0, 0.5),
            ("grok", "grok-4.7", 2.0, 0.5),
            ("grok", "some-future-grok", 2.0, 0.3),
        ] {
            let want = (8_000.0 * input + 4_000.0 * cached) / 1e6;
            let got = cost_usd(group, model, &tokens, &empty());
            assert!((got - want).abs() < 1e-12, "{model}: {got} != {want}");
        }
    }

    #[test]
    fn gpt_5_6_sol_matches_exact_and_bare_and_prefix() {
        // Exact `gpt-5.6-sol`, the bare `gpt-5.6` alias, and a future dated
        // snapshot all resolve to sol rates ($4 in / $20 out, OpenAI pricing
        // page 2026-10-06).
        for model in ["gpt-5.6-sol", "gpt-5.6", "gpt-5.6-sol-20260709"] {
            let cost = cost_usd("codex", model, &tc(1_000_000, 0, None, None), &empty());
            approx(cost, 4.00);
            let out = cost_usd("codex", model, &tc(0, 1_000_000, None, None), &empty());
            approx(out, 20.00);
        }
    }

    #[test]
    fn gpt_6_astra_matches_exact_and_bare_and_prefix() {
        // Exact `gpt-6-astra`, the bare `gpt-6` alias, and a dated snapshot
        // all resolve to astra rates ($10 in / $50 out / $1 cached input).
        for model in ["gpt-6-astra", "gpt-6", "gpt-6-astra-20260903"] {
            let cost = cost_usd("codex", model, &tc(1_000_000, 0, None, None), &empty());
            approx(cost, 10.00);
            let out = cost_usd("codex", model, &tc(0, 1_000_000, None, None), &empty());
            approx(out, 50.00);
            let cached = cost_usd("codex", model, &tc(0, 0, Some(1_000_000), None), &empty());
            approx(cached, 1.00);
        }
        // OpenAI lists a cache-write rate for astra ($12.50, 2026-10-06).
        let creation = cost_usd(
            "codex",
            "gpt-6-astra",
            &tc(0, 0, None, Some(1_000_000)),
            &empty(),
        );
        approx(creation, 12.50);
        // The bare ALIAS is deliberately absent from the price table: the
        // codex provider resolves `astra` to `gpt-6-astra` BEFORE the request
        // (and the recorded model) leaves llmux, so pricing only ever sees the
        // resolved slug. Pricing the alias too would be a second source of
        // truth that could silently disagree with the wire model.
        assert_eq!(builtin_price("astra"), None);
        assert_eq!(
            price_for("codex", "astra", &empty()),
            Some(GPT_5_5_FLAT),
            "an unresolved alias would fall back to the codex group rate"
        );
    }

    #[test]
    fn gpt_60_and_gpt_6_5_do_not_resolve_to_astra_pricing() {
        // Generation boundary, same shape as the 5.60 test: a wider `gpt-60-`
        // or a newer `gpt-6.5-` generation is NOT gpt-6, so both miss the
        // built-in table and land on the codex group fallback (gpt-5.5 rates).
        assert_eq!(builtin_price("gpt-60-astra"), None);
        assert_eq!(builtin_price("gpt-6.5-astra"), None);
        assert_eq!(
            price_for("codex", "gpt-60-astra", &empty()),
            Some(GPT_5_5_FLAT),
            "unknown codex model takes the group fallback"
        );
        // The boundary must not break the real family ids.
        assert_eq!(builtin_price("gpt-6"), Some(GPT_6_ASTRA));
        assert_eq!(builtin_price("gpt-6-astra"), Some(GPT_6_ASTRA));
        assert_eq!(builtin_price("gpt-6-astra-20260903"), Some(GPT_6_ASTRA));
    }

    #[test]
    fn gpt_5_6_terra_and_luna_have_tier_rates() {
        let terra = cost_usd(
            "codex",
            "gpt-5.6-terra",
            &tc(1_000_000, 0, None, None),
            &empty(),
        );
        // Model page, read 2026-10-23: $2 in / $0.20 cached / $12 out.
        approx(terra, 2.0);
        // Pricing page, read 2026-11-11: the 30m write column is $2.50 for
        // terra and $0.25 for luna (1.25x input, as the model pages state).
        let rates = |model: &str| {
            let p = price_for("codex", model, &empty()).expect("priced");
            (p.input, p.output, p.cache_read, p.cache_creation)
        };
        assert_eq!(rates("gpt-5.6-terra"), (2.0, 12.0, 0.2, 2.5));
        assert_eq!(rates("gpt-5.6-luna"), (0.20, 1.20, 0.02, 0.25));
        let luna = cost_usd(
            "codex",
            "gpt-5.6-luna",
            &tc(0, 1_000_000, None, None),
            &empty(),
        );
        approx(luna, 1.20);
        approx(
            cost_usd(
                "codex",
                "gpt-5.6-terra",
                &tc(0, 0, None, Some(1_000_000)),
                &empty(),
            ),
            2.5,
        );
    }

    #[test]
    fn gpt_5_6_sol_cache_creation_is_five_dollars() {
        // OpenAI's pricing page lists a cache-write rate since the 2026-10-06
        // re-read; the old "codex has no cache-creation charge" row is gone.
        let cost = cost_usd(
            "codex",
            "gpt-5.6-sol",
            &tc(0, 0, None, Some(1_000_000)),
            &empty(),
        );
        approx(cost, 5.0);
    }

    #[test]
    fn sonnet_5_and_5_5_take_the_two_dollar_row_not_the_sonnet_tier() {
        // Both generations are $2 / $10 / $2.50 writes; Sonnet 5.5 cache
        // reads are now $0.10 vs Sonnet 5 $0.20. The 4.x ids keep $3 / $15; the
        // generic prefix fallback still lands there.
        for model in [
            "claude-sonnet-5-5",
            "claude-sonnet-5",
            "claude-sonnet-5-5[1m]",
        ] {
            let cost = cost_usd("claude", model, &tc(1_000_000, 0, None, None), &empty());
            approx(cost, 2.00);
            let out = cost_usd("claude", model, &tc(0, 1_000_000, None, None), &empty());
            approx(out, 10.00);
            let cr = cost_usd("claude", model, &tc(0, 0, Some(1_000_000), None), &empty());
            approx(
                cr,
                if model == "claude-sonnet-5" {
                    0.20
                } else {
                    0.10
                },
            );
            let cw = cost_usd("claude", model, &tc(0, 0, None, Some(1_000_000)), &empty());
            approx(cw, 2.50);
        }
        // The `sonnet` alias resolves to sonnet-5-5 before pricing.
        approx(
            cost_usd("claude", "sonnet", &tc(1_000_000, 0, None, None), &empty()),
            2.00,
        );
        approx(
            cost_usd(
                "claude",
                "claude-sonnet-4-6",
                &tc(1_000_000, 0, None, None),
                &empty(),
            ),
            3.00,
        );
        assert_eq!(builtin_price("claude-sonnet-50-1"), Some(SONNET_TIER));
    }

    #[test]
    fn gpt_6_1_sol_and_gen6_tiers_have_their_own_rates() {
        // gpt-6.1-sol: $2 / $0.10 cached / $2.50 write / $10 (pricing page
        // 2026-10-06); bare `gpt-6.1` and a dated snapshot resolve the same.
        for model in ["gpt-6.1-sol", "gpt-6.1", "gpt-6.1-sol-20260929"] {
            approx(
                cost_usd("codex", model, &tc(1_000_000, 0, None, None), &empty()),
                2.00,
            );
            approx(
                cost_usd("codex", model, &tc(0, 1_000_000, None, None), &empty()),
                10.00,
            );
            approx(
                cost_usd("codex", model, &tc(0, 0, Some(1_000_000), None), &empty()),
                0.10,
            );
            approx(
                cost_usd("codex", model, &tc(0, 0, None, Some(1_000_000)), &empty()),
                2.50,
            );
        }
        // gpt-6-sol: $2 / $0.20 / $2.50 / $10 (model page); gpt-6-luna:
        // $0.10 / $0.01 / $0.125 / $0.50 (pricing page).
        approx(
            cost_usd(
                "codex",
                "gpt-6-sol",
                &tc(0, 0, Some(1_000_000), None),
                &empty(),
            ),
            0.20,
        );
        approx(
            cost_usd(
                "codex",
                "gpt-6-sol-20260922",
                &tc(0, 1_000_000, None, None),
                &empty(),
            ),
            10.00,
        );
        approx(
            cost_usd(
                "codex",
                "gpt-6-luna",
                &tc(1_000_000, 0, None, None),
                &empty(),
            ),
            0.10,
        );
        approx(
            cost_usd(
                "codex",
                "gpt-6-luna",
                &tc(0, 1_000_000, None, None),
                &empty(),
            ),
            0.50,
        );
        approx(
            cost_usd(
                "codex",
                "gpt-6-luna-20260922",
                &tc(0, 0, None, Some(1_000_000)),
                &empty(),
            ),
            0.125,
        );
        // Generation boundary: `gpt-6.10-*` is not 6.1.
        assert_eq!(builtin_price("gpt-6.10-sol"), None);
        // The bare `gpt-6-` prefix still means astra.
        assert_eq!(builtin_price("gpt-6-terra-x"), Some(GPT_6_ASTRA));
    }

    #[test]
    fn fable_5_2_takes_its_own_rates_not_the_fable_tier() {
        // Fable 5.2 (pricing page, read 2026-11-10): $8 in / $40 out / $0.20
        // cache read / $10 cache write (5m). The `fable` alias, the version
        // alias, the `[1m]` catalog id and a dated snapshot all price here.
        let fable_5_2 = ModelPrice::new(8.0, 40.0, 0.20, 10.0).with_extended(16.0);
        for model in [
            "claude-fable-5-2",
            "claude-fable-5-2[1m]",
            "CLAUDE-FABLE-5-2",
            "fable",
            "fable[1m]",
            "fable-5-2",
            "claude-fable-5-2-20261109",
        ] {
            assert_eq!(
                price_for("claude", model, &empty()),
                Some(fable_5_2),
                "{model}"
            );
        }
        for (tokens, expected) in [
            (tc(1_000_000, 0, None, None), 8.0),
            (tc(0, 1_000_000, None, None), 40.0),
            (tc(0, 0, Some(1_000_000), None), 0.20),
            (tc(0, 0, None, Some(1_000_000)), 10.0),
        ] {
            approx(
                cost_usd("claude", "claude-fable-5-2", &tokens, &empty()),
                expected,
            );
        }
        // Version boundary: `claude-fable-5-20-*` and `claude-fable-5-2x` are
        // different models and must NOT take 5.2 rates — they fall to the
        // Fable tier, like any unknown Fable id.
        assert_eq!(
            builtin_price("claude-fable-5-20-20261109"),
            Some(FABLE_TIER)
        );
        assert_eq!(builtin_price("claude-fable-5-20"), Some(FABLE_TIER));
        assert_eq!(builtin_price("claude-fable-5-2x"), Some(FABLE_TIER));
        // Fable 5.1 keeps its own row, exact and dated.
        assert_eq!(builtin_price("claude-fable-5-1"), Some(FABLE_5_1));
        assert_eq!(builtin_price("claude-fable-5-1-20260901"), Some(FABLE_5_1));
    }

    #[test]
    fn mythos_rows_take_their_own_rates_not_the_opus_fallback() {
        // Pricing page, read 2026-11-11: Mythos 5.2 $8 / $40 / $0.20 / $10,
        // Mythos 5.1 $10 / $50 / $0.25 / $12.50, Mythos 5 $10 / $50 / $1 /
        // $12.50 (input / output / cache read / 5m cache write); 1h writes $16 /
        // $20 / $20.
        for (model, want) in [
            (
                "claude-mythos-5-2",
                ModelPrice::new(8.0, 40.0, 0.20, 10.0).with_extended(16.0),
            ),
            (
                "claude-mythos-5-2[1m]",
                ModelPrice::new(8.0, 40.0, 0.20, 10.0).with_extended(16.0),
            ),
            (
                "claude-mythos-5-2-20261109",
                ModelPrice::new(8.0, 40.0, 0.20, 10.0).with_extended(16.0),
            ),
            (
                "claude-mythos-5-1",
                ModelPrice::new(10.0, 50.0, 0.25, 12.5).with_extended(20.0),
            ),
            (
                "claude-mythos-5-1-20260901",
                ModelPrice::new(10.0, 50.0, 0.25, 12.5).with_extended(20.0),
            ),
            (
                "claude-mythos-5",
                ModelPrice::new(10.0, 50.0, 1.0, 12.5).with_extended(20.0),
            ),
            (
                "claude-mythos-5[1m]",
                ModelPrice::new(10.0, 50.0, 1.0, 12.5).with_extended(20.0),
            ),
        ] {
            assert_eq!(price_for("claude", model, &empty()), Some(want), "{model}");
        }
        // Version boundary, as for Fable: `claude-mythos-5-20` is not 5.2.
        let mythos_5 = Some(ModelPrice::new(10.0, 50.0, 1.0, 12.5).with_extended(20.0));
        assert_eq!(builtin_price("claude-mythos-5-20"), mythos_5);
        assert_eq!(builtin_price("claude-mythos-5-2x"), mythos_5);
    }

    #[test]
    fn gpt_6_1_terra_takes_its_own_rates_not_sol() {
        // gpt-6.1-terra (model page + pricing page, read 2026-11-10; standard,
        // ≤272k input): $1 in / $0.05 cached / $1.25 cache write (30m) / $5
        // out. Exact, `[1m]` and dated ids price here, not at 6.1-sol.
        for model in [
            "gpt-6.1-terra",
            "gpt-6.1-terra[1m]",
            "GPT-6.1-TERRA",
            "gpt-6.1-terra-20261026",
        ] {
            assert_eq!(
                price_for("codex", model, &empty()),
                Some(GPT_6_1_TERRA),
                "{model}"
            );
        }
        for (tokens, expected) in [
            (tc(1_000_000, 0, None, None), 1.0),
            (tc(0, 1_000_000, None, None), 5.0),
            (tc(0, 0, Some(1_000_000), None), 0.05),
            (tc(0, 0, None, Some(1_000_000)), 1.25),
        ] {
            approx(
                cost_usd("codex", "gpt-6.1-terra", &tokens, &empty()),
                expected,
            );
        }
        // Sol keeps the `gpt-6.1-` default: a terra look-alike without the
        // `-` boundary is not terra, and `gpt-6.10-terra` is not 6.1 at all.
        assert_eq!(builtin_price("gpt-6.1-terrax"), Some(GPT_6_1_SOL));
        assert_eq!(builtin_price("gpt-6.1-sol"), Some(GPT_6_1_SOL));
        assert_eq!(builtin_price("gpt-6.1"), Some(GPT_6_1_SOL));
        assert_eq!(builtin_price("gpt-6.10-terra"), None);
        // The 5.6 terra row is unchanged.
        assert_eq!(builtin_price("gpt-5.6-terra"), Some(GPT_5_6_TERRA));
    }

    #[test]
    fn gpt_5_60_does_not_resolve_to_gpt_5_6_pricing() {
        // Generation boundary: `gpt-5.60-sol` is NOT a gpt-5.6 model. It must
        // miss the built-in table entirely (no bare `gpt-5.6` prefix match)
        // and land on the codex group fallback (gpt-5.5 rates), same as any
        // other unknown codex model.
        assert_eq!(builtin_price("gpt-5.60-sol"), None);
        assert_eq!(builtin_price("gpt-5.60-terra"), None);
        assert_eq!(builtin_price("gpt-5.50-mini"), None);
        assert_eq!(
            price_for("codex", "gpt-5.60-sol", &empty()),
            Some(GPT_5_5_FLAT),
            "unknown codex model takes the group fallback"
        );
        // The boundary must not break real dated snapshots of the family.
        assert_eq!(builtin_price("gpt-5.6-sol-20260709"), Some(GPT_5_6_SOL));
        assert_eq!(builtin_price("gpt-5.6-terra-20260709"), Some(GPT_5_6_TERRA));
        assert_eq!(builtin_price("gpt-5.6-luna-20260709"), Some(GPT_5_6_LUNA));
        assert_eq!(builtin_price("gpt-5.5-codex"), Some(GPT_5_5_FLAT));
    }

    #[test]
    fn gpt_5_6_sol_cache_read_is_ten_percent_of_input() {
        // OpenAI bills cached input at the flat gpt-5.x discount (10% of the
        // input rate). gpt-5.6-sol input is $4/1e6 (pricing page 2026-10-06),
        // so 1e6 cache-read tokens cost $0.40 — a third of a mostly-cached
        // prompt is billed at a tenth, not the full input rate (the codex
        // cache-read cost regression).
        let cache = cost_usd(
            "codex",
            "gpt-5.6-sol",
            &tc(0, 0, Some(1_000_000), None),
            &empty(),
        );
        approx(cache, 0.40);
        let input = cost_usd(
            "codex",
            "gpt-5.6-sol",
            &tc(1_000_000, 0, None, None),
            &empty(),
        );
        approx(cache, input * 0.10);
    }

    #[test]
    fn mixed_tokens_sum_each_component() {
        // opus: 5/25/0.5/6.25 per 1e6.
        // 200k in (1.0) + 100k out (2.5) + 50k cr (0.025) + 40k cc (0.25) = 3.775.
        let cost = cost_usd(
            "claude",
            "claude-opus-4-8",
            &tc(200_000, 100_000, Some(50_000), Some(40_000)),
            &empty(),
        );
        approx(cost, 1.0 + 2.5 + 0.025 + 0.25);
    }

    #[test]
    fn sonnet_and_haiku_and_fable_tiers() {
        approx(
            cost_usd(
                "claude",
                "claude-sonnet-4-5",
                &tc(1_000_000, 0, None, None),
                &empty(),
            ),
            3.0,
        );
        approx(
            cost_usd(
                "claude",
                "claude-haiku-4-5",
                &tc(0, 1_000_000, None, None),
                &empty(),
            ),
            5.0,
        );
        approx(
            cost_usd(
                "claude",
                "claude-fable-5",
                &tc(1_000_000, 0, None, None),
                &empty(),
            ),
            10.0,
        );
        approx(
            cost_usd(
                "claude",
                "claude-fable-5-1",
                &tc(1_000_000, 0, None, None),
                &empty(),
            ),
            10.0,
        );
    }

    #[test]
    fn normalized_suffix_resolves_to_same_price() {
        // The display-only [1m] suffix must not split the price lookup.
        let bare = cost_usd(
            "claude",
            "claude-opus-4-8",
            &tc(1_000_000, 0, None, None),
            &empty(),
        );
        let suffixed = cost_usd(
            "claude",
            "claude-opus-4-8[1m]",
            &tc(1_000_000, 0, None, None),
            &empty(),
        );
        approx(bare, 5.0);
        approx(suffixed, 5.0);
    }

    #[test]
    fn case_insensitive_model_lookup() {
        let cost = cost_usd(
            "claude",
            "Claude-Opus-4-8",
            &tc(1_000_000, 0, None, None),
            &empty(),
        );
        approx(cost, 5.0);
    }

    #[test]
    fn unknown_model_empty_overrides_unknown_group_is_zero_no_panic() {
        let cost = cost_usd(
            "weirdgroup",
            "totally-made-up-model",
            &tc(9_999_999, 9_999_999, Some(9_999_999), Some(9_999_999)),
            &empty(),
        );
        approx(cost, 0.0);
    }

    #[test]
    fn unknown_claude_model_falls_back_to_opus_tier() {
        let cost = cost_usd(
            "claude",
            "claude-future-9",
            &tc(1_000_000, 0, None, None),
            &empty(),
        );
        approx(cost, 5.0);
    }

    #[test]
    fn unknown_codex_model_falls_back_to_gpt_5_5() {
        // A slug from no known generation. (`gpt-6-mini` was the old sample;
        // since 2026-09-07 `gpt-6-` is a REAL generation prefix defaulting to
        // the astra flagship, exactly like `gpt-5.6-` defaults to sol.)
        let cost = cost_usd(
            "codex",
            "gpt-7-mini",
            &tc(0, 1_000_000, None, None),
            &empty(),
        );
        approx(cost, 30.0);
    }

    #[test]
    fn config_override_beats_default() {
        let mut overrides = HashMap::new();
        overrides.insert("gpt-5.5".to_string(), ModelPrice::new(9.99, 0.0, 0.0, 0.0));
        let cost = cost_usd(
            "codex",
            "gpt-5.5",
            &tc(1_000_000, 0, None, None),
            &overrides,
        );
        approx(cost, 9.99);
    }

    #[test]
    fn config_override_keyed_with_suffix_still_matches() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "claude-opus-4-8[1m]".to_string(),
            ModelPrice::new(1.0, 0.0, 0.0, 0.0),
        );
        // Looked up by the bare slug; override key carried a [1m] suffix.
        let cost = cost_usd(
            "claude",
            "claude-opus-4-8",
            &tc(1_000_000, 0, None, None),
            &overrides,
        );
        approx(cost, 1.0);
    }

    #[test]
    fn priced_cost_matches_cost_usd() {
        let tokens = tc(700, 300, Some(120), None);
        let a = cost_usd("codex", "gpt-5.5", &tokens, &empty());
        let b = priced_cost("codex", "gpt-5.5", &tokens, &empty()).expect("priced");
        approx(a, b);
        // gpt-5.5: 700*5/1e6 + 300*30/1e6 + 120*0.5/1e6 = 0.0035 + 0.009 + 0.00006.
        approx(b, 0.0035 + 0.009 + 0.000_06);
    }

    #[test]
    fn price_for_returns_none_for_unknown_group_and_model() {
        assert!(price_for("nope", "nope", &empty()).is_none());
        // But an override makes even an unknown group resolvable.
        let mut overrides = HashMap::new();
        overrides.insert("nope".to_string(), ModelPrice::zero());
        assert!(price_for("nope", "nope", &overrides).is_some());
    }

    fn split(cc: u64, extended: Option<u64>) -> TokenCounts {
        TokenCounts {
            cache_creation: Some(cc),
            cache_creation_extended: extended,
            ..Default::default()
        }
    }

    #[test]
    fn extended_ttl_writes_take_the_extended_rate() {
        // 5m part = cache_creation - extended at the cache-write rate, the
        // extended part at its own rate (#278). Opus 5.5: $5 (5m) / $8 (1h).
        approx(
            cost_usd(
                "claude",
                "claude-opus-5-5",
                &split(1_000_000, Some(600_000)),
                &empty(),
            ),
            0.4 * 5.0 + 0.6 * 8.0,
        );
        // All 1h; and a split above the total is clamped to it.
        for extended in [1_000_000, 2_000_000] {
            approx(
                cost_usd(
                    "claude",
                    "claude-opus-5-5",
                    &split(1_000_000, Some(extended)),
                    &empty(),
                ),
                8.0,
            );
        }
        // OpenAI 12h writes: gpt-5.6-terra $2.50 (30m) / $5 (12h).
        approx(
            cost_usd(
                "codex",
                "gpt-5.6-terra",
                &split(1_000_000, Some(1_000_000)),
                &empty(),
            ),
            5.0,
        );
        // A row with no separate write charge (gpt-5.5, grok: written tokens
        // bill as ordinary input) prices an extended write at its input-rate
        // write rate too, never 0.
        for (group, model, input) in [
            ("codex", "gpt-5.5", 5.0),
            ("codex", "some-future-codex-model", 5.0),
            ("grok", "grok-4.7", 2.0),
        ] {
            approx(
                cost_usd(group, model, &split(1_000_000, Some(400_000)), &empty()),
                input,
            );
        }
    }

    #[test]
    fn the_extended_write_rates_match_the_pricing_pages() {
        // platform.claude.com/docs/en/about-claude/pricing "1h cache writes"
        // (2x input) and developers.openai.com/api/docs/pricing "12h write"
        // (2.5x input), both read 2026-11-11.
        for (group, model, want) in [
            ("claude", "claude-fable-5-2", 16.0),
            ("claude", "claude-fable-5-1", 20.0),
            ("claude", "claude-fable-5", 20.0),
            ("claude", "claude-mythos-5-2", 16.0),
            ("claude", "claude-mythos-5-1", 20.0),
            ("claude", "claude-mythos-5", 20.0),
            ("claude", "claude-opus-5-5", 8.0),
            ("claude", "claude-opus-5", 10.0),
            ("claude", "claude-opus-4-8", 10.0),
            ("claude", "claude-opus-4-5", 10.0),
            ("claude", "claude-sonnet-5-5", 4.0),
            ("claude", "claude-sonnet-5", 4.0),
            ("claude", "claude-sonnet-4-6", 6.0),
            ("claude", "claude-sonnet-4-5", 6.0),
            ("claude", "claude-haiku-5-5", 0.20),
            ("claude", "claude-haiku-4-5", 2.0),
            ("codex", "gpt-5.6-sol", 10.0),
            ("codex", "gpt-5.6-terra", 5.0),
            ("codex", "gpt-5.6-luna", 0.50),
            ("codex", "gpt-6-astra", 25.0),
            ("codex", "gpt-6-sol", 5.0),
            ("codex", "gpt-6-luna", 0.25),
            ("codex", "gpt-6.1-sol", 5.0),
            ("codex", "gpt-6.1-terra", 2.50),
        ] {
            let price = price_for(group, model, &empty()).expect("priced");
            assert_eq!(price.cache_creation_extended, Some(want), "{model}");
        }
        // No extended rate (= the row's write rate): gpt-5.5 and grok (no
        // separate write charge, so the input rate at any TTL), gemini.
        for (group, model) in [
            ("codex", "gpt-5.5"),
            ("grok", "grok-4.7"),
            ("gemini", "gemini-4-argon"),
        ] {
            let price = price_for(group, model, &empty()).expect("priced");
            assert_eq!(price.cache_creation_extended, None, "{model}");
        }
    }

    #[test]
    fn a_request_without_a_split_or_searches_prices_bit_for_bit_as_before() {
        // The pre-#278 formula, applied to every row family and to an
        // override: no split (or a zero one) and no searches change nothing.
        let before = |p: ModelPrice, t: &TokenCounts| {
            let per_m = |count: u64, rate: f64| (count as f64) * rate / 1_000_000.0;
            per_m(t.input, p.input)
                + per_m(t.output, p.output)
                + per_m(t.cache_read.unwrap_or(0), p.cache_read)
                + per_m(t.cache_creation.unwrap_or(0), p.cache_creation)
        };
        let override_price = ModelPrice::new(1.1, 2.2, 0.33, 4.4);
        let overrides = HashMap::from([("claude-opus-4-8".to_string(), override_price)]);
        for (group, model) in [
            ("claude", "claude-opus-5-5"),
            ("claude", "claude-opus-4-8"),
            ("claude", "claude-haiku-5-5"),
            ("claude", "claude-fable-5-2"),
            ("codex", "gpt-5.5"),
            ("codex", "gpt-6.1-terra"),
            ("grok", "grok-4.7"),
            ("gemini", "gemini-3.8-flash"),
        ] {
            for extended in [None, Some(0)] {
                let t = TokenCounts {
                    input: 123_457,
                    output: 9_876,
                    cache_read: Some(55_555),
                    cache_creation: Some(7_777),
                    cache_creation_extended: extended,
                    web_search_requests: None,
                };
                for overrides in [&empty(), &overrides] {
                    let price = price_for(group, model, overrides).expect("priced");
                    assert_eq!(
                        cost_usd(group, model, &t, overrides).to_bits(),
                        before(price, &t).to_bits(),
                        "{model} {extended:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn an_override_without_the_extended_rate_prices_those_writes_at_its_write_rate() {
        // A config override written before #278 has no extended rate: its
        // extended writes cost the override's own cache-write rate, never the
        // built-in 1h rate.
        let legacy: ModelPrice = serde_json::from_str(
            r#"{"input":1.0,"output":2.0,"cache_read":0.1,"cache_creation":1.25}"#,
        )
        .expect("legacy override parses");
        assert_eq!(legacy.cache_creation_extended, None);
        assert_eq!(
            serde_json::to_string(&legacy).expect("serialize"),
            r#"{"input":1.0,"output":2.0,"cache_read":0.1,"cache_creation":1.25}"#,
            "re-serializes unchanged"
        );
        let overrides = HashMap::from([("claude-opus-5-5".to_string(), legacy)]);
        approx(
            cost_usd(
                "claude",
                "claude-opus-5-5",
                &split(1_000_000, Some(1_000_000)),
                &overrides,
            ),
            1.25,
        );
        // An override may set it.
        let with: ModelPrice = serde_json::from_str(
            r#"{"input":1.0,"output":2.0,"cache_read":0.1,"cache_creation":1.25,"cache_creation_extended":2.0}"#,
        )
        .expect("override parses");
        let overrides = HashMap::from([("claude-opus-5-5".to_string(), with)]);
        approx(
            cost_usd(
                "claude",
                "claude-opus-5-5",
                &split(1_000_000, Some(400_000)),
                &overrides,
            ),
            0.6 * 1.25 + 0.4 * 2.0,
        );
    }

    #[test]
    fn anthropic_web_searches_cost_ten_dollars_per_thousand() {
        // platform.claude.com/docs/en/about-claude/pricing ("Web search
        // tool", read 2026-11-11): $10 per 1,000 searches plus tokens; web
        // fetch has no additional charge (not counted).
        let searches = |n: u64| TokenCounts {
            input: 1_000_000,
            web_search_requests: Some(n),
            ..Default::default()
        };
        approx(
            cost_usd("claude", "claude-opus-5-5", &searches(3), &empty()),
            4.0 + 0.03,
        );
        // An override changes the token rates, not the per-search fee.
        let overrides = HashMap::from([(
            "claude-opus-5-5".to_string(),
            ModelPrice::new(1.0, 0.0, 0.0, 0.0),
        )]);
        approx(
            cost_usd("claude", "claude-opus-5-5", &searches(3), &overrides),
            1.0 + 0.03,
        );
        // Only Anthropic's fee is modeled.
        approx(cost_usd("codex", "gpt-5.5", &searches(3), &empty()), 5.0);
        assert_eq!(
            priced_cost("openrouter", "or-x/unknown", &searches(3), &empty()),
            None
        );
    }

    // ---- #278 PR 3: per-request price classes ----

    fn prompt(input: u64, cache_read: u64, cache_creation: u64) -> TokenCounts {
        TokenCounts {
            input,
            cache_read: Some(cache_read),
            cache_creation: Some(cache_creation),
            ..Default::default()
        }
    }

    const NOV_11: RateDay = RateDay::ymd(2026, 11, 11);

    fn long() -> PriceClass {
        PriceClass {
            long_context: true,
            ..PriceClass::STANDARD
        }
    }

    fn fast() -> PriceClass {
        PriceClass {
            fast: true,
            ..PriceClass::STANDARD
        }
    }

    fn rates(input: f64, output: f64, cr: f64, cc: f64, ext: Option<f64>) -> TierRates {
        TierRates {
            input,
            output,
            cache_read: cr,
            cache_creation: cc,
            cache_creation_extended: ext,
        }
    }

    #[test]
    fn long_context_boundaries_follow_each_pricing_page() {
        let is_long = |group: &str, model: &str, t: TokenCounts| {
            PriceClass::STANDARD
                .resolve(group, model, &t, false, NOV_11)
                .long_context
        };
        // OpenAI: "Long context: >272K input tokens"
        // (developers.openai.com/api/docs/pricing, read 2026-11-11). The
        // prompt counts fresh input, cache reads and cache writes.
        for model in [
            "gpt-6.1-terra",
            "gpt-6.1-terra-20261026",
            "gpt-6.1-sol",
            "gpt-6.1",
            "gpt-6-astra",
            "gpt-6",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-5.6-sol",
            "gpt-5.6",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
        ] {
            assert!(!is_long("codex", model, prompt(272_000, 0, 0)), "{model}");
            assert!(
                !is_long("codex", model, prompt(100_000, 100_000, 72_000)),
                "{model}"
            );
            assert!(is_long("codex", model, prompt(272_001, 0, 0)), "{model}");
            assert!(
                is_long("codex", model, prompt(1, 200_000, 72_000)),
                "{model}"
            );
        }
        // xAI: "(≥ 200k prompt tokens)" (docs.x.ai/developers/pricing, read
        // 2026-11-11).
        for model in ["grok-4.5", "grok-4.6", "grok-4.7"] {
            assert!(!is_long("grok", model, prompt(199_999, 0, 0)), "{model}");
            assert!(is_long("grok", model, prompt(200_000, 0, 0)), "{model}");
            assert!(
                is_long("grok", model, prompt(0, 150_000, 50_000)),
                "{model}"
            );
        }
        // Anthropic: Haiku 5.5 "for prompts over 100,000 tokens", the prompt
        // "counts all of its input tokens, including cache reads and cache
        // writes" (platform.claude.com/docs/en/about-claude/pricing, read
        // 2026-11-11).
        for model in ["claude-haiku-5-5", "haiku", "claude-haiku-5-5-20261007"] {
            assert!(!is_long("claude", model, prompt(100_000, 0, 0)), "{model}");
            assert!(is_long("claude", model, prompt(100_001, 0, 0)), "{model}");
            assert!(
                is_long("claude", model, prompt(1, 60_000, 40_000)),
                "{model}"
            );
        }
        // No tier: every other Claude row ("the full 1M token context window
        // at standard pricing"), Haiku 4.5, codex ids the page does not list,
        // the codex group fallback, gemini.
        for (group, model) in [
            ("claude", "claude-opus-5-5"),
            ("claude", "claude-sonnet-5-5"),
            ("claude", "claude-fable-5-2"),
            ("claude", "claude-haiku-4-5"),
            ("codex", "gpt-5.5-codex"),
            ("codex", "gpt-7-mini"),
            ("gemini", "gemini-4-argon"),
        ] {
            assert!(!is_long(group, model, prompt(900_000, 0, 0)), "{model}");
        }
    }

    #[test]
    fn long_context_rates_match_the_pricing_pages() {
        // (group, model, long input / output / cached / write / extended
        // write). OpenAI long rows of the Standard table, xAI "≥ 200k" rows,
        // the Haiku 5.5 "over 100,000" row — all read 2026-11-11.
        for (group, model, want) in [
            (
                "codex",
                "gpt-6.1-terra",
                rates(2.0, 7.5, 0.10, 2.5, Some(5.0)),
            ),
            (
                "codex",
                "gpt-6.1-sol",
                rates(4.0, 15.0, 0.20, 5.0, Some(10.0)),
            ),
            (
                "codex",
                "gpt-6-astra",
                rates(20.0, 75.0, 2.0, 25.0, Some(50.0)),
            ),
            (
                "codex",
                "gpt-6-sol",
                rates(4.0, 15.0, 0.40, 5.0, Some(10.0)),
            ),
            (
                "codex",
                "gpt-6-luna",
                rates(0.20, 0.75, 0.02, 0.25, Some(0.50)),
            ),
            (
                "codex",
                "gpt-5.6-sol",
                rates(8.0, 30.0, 0.80, 10.0, Some(20.0)),
            ),
            (
                "codex",
                "gpt-5.6-terra",
                rates(4.0, 18.0, 0.40, 5.0, Some(10.0)),
            ),
            (
                "codex",
                "gpt-5.6-luna",
                rates(0.40, 1.80, 0.04, 0.50, Some(1.0)),
            ),
            // No write charge on gpt-5.5: a written token bills as input.
            ("codex", "gpt-5.5", rates(10.0, 45.0, 1.0, 10.0, None)),
            ("grok", "grok-4.5", rates(4.0, 12.0, 0.60, 4.0, None)),
            ("grok", "grok-4.6", rates(4.0, 12.0, 1.0, 4.0, None)),
            ("grok", "grok-4.7", rates(4.0, 12.0, 1.0, 4.0, None)),
            (
                "claude",
                "claude-haiku-5-5",
                rates(0.50, 2.50, 0.05, 0.625, Some(1.0)),
            ),
        ] {
            let price = price_for(group, model, &empty()).expect("priced");
            assert_eq!(price.rates(long()), want, "{model}");
        }
        // The WHOLE request bills at the long rates: 300k fresh input on
        // gpt-6.1-terra is 300k x $2.00, not 272k x $1 + 28k x $2.
        approx(
            class_cost(
                "codex",
                "gpt-6.1-terra",
                long(),
                &prompt(300_000, 0, 0),
                &empty(),
            )
            .expect("priced"),
            0.6,
        );
        // Haiku 5.5: 5x every rate, 5m and 1h writes included.
        let haiku = TokenCounts {
            input: 50_000,
            output: 1_000,
            cache_read: Some(40_000),
            cache_creation: Some(20_000),
            cache_creation_extended: Some(5_000),
            web_search_requests: None,
        };
        approx(
            class_cost("claude", "claude-haiku-5-5", long(), &haiku, &empty()).expect("priced"),
            (50_000.0 * 0.50 + 1_000.0 * 2.50 + 40_000.0 * 0.05 + 15_000.0 * 0.625 + 5_000.0 * 1.0)
                / 1e6,
        );
    }

    #[test]
    fn fast_tier_rates_match_the_pricing_page() {
        // The "Fast (formerly Priority)" table of
        // https://developers.openai.com/api/docs/pricing, read 2026-11-11:
        // 2x Standard on the 6.x and 5.6 rows, short and long context.
        let fast_long = PriceClass {
            long_context: true,
            fast: true,
            ..PriceClass::STANDARD
        };
        for model in [
            "gpt-6.1-terra",
            "gpt-6.1-sol",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
        ] {
            let price = price_for("codex", model, &empty()).expect("priced");
            assert_eq!(price.rates(fast()), price.standard().times(2.0), "{model}");
            assert_eq!(
                price.rates(fast_long),
                price.rates(long()).times(2.0),
                "{model}"
            );
        }
        // Spot-check page cells: gpt-6-astra Fast long $40 / $4 / $50 / $100
        // / $150; gpt-6.1-terra Fast short $2 / $0.10 / $2.50 / $5 / $10.
        let astra = price_for("codex", "gpt-6-astra", &empty()).expect("priced");
        assert_eq!(
            astra.rates(fast_long),
            rates(40.0, 150.0, 4.0, 50.0, Some(100.0))
        );
        let terra = price_for("codex", "gpt-6.1-terra", &empty()).expect("priced");
        assert_eq!(terra.rates(fast()), rates(2.0, 10.0, 0.10, 2.5, Some(5.0)));
        // gpt-5.5 Fast is 2.5x: $12.50 / $1.25 / $75 (writes bill as input).
        // The page lists no long-context Fast rate for gpt-5.5 (every cell
        // "-"), so a long Fast gpt-5.5 request prices at the published long
        // Standard rates rather than an invented number.
        let gpt_5_5 = price_for("codex", "gpt-5.5", &empty()).expect("priced");
        assert_eq!(gpt_5_5.rates(fast()), rates(12.5, 75.0, 1.25, 12.5, None));
        assert_eq!(gpt_5_5.rates(fast_long), rates(10.0, 45.0, 1.0, 10.0, None));
        // A Fast request is classified from the recorded flag.
        assert!(
            PriceClass::STANDARD
                .resolve("codex", "gpt-6.1-sol", &prompt(10, 0, 0), true, NOV_11)
                .fast
        );
        approx(
            class_cost(
                "codex",
                "gpt-6.1-sol",
                fast(),
                &prompt(1_000_000, 0, 0),
                &empty(),
            )
            .expect("priced"),
            4.0,
        );
        // Codex ids the page does not list keep a flat price.
        let codex_fallback = price_for("codex", "gpt-7-mini", &empty()).expect("fallback");
        assert_eq!(codex_fallback.rates(fast()), codex_fallback.standard());
    }

    #[test]
    fn dated_rates_follow_the_request_day() {
        // https://aistudio.google.com/docs/pricing (read 2026-11-11): Gemini
        // 4 Argon "$2.00 through November 30, 2026" / "$4.00 starting
        // December 1, 2026" (output $10 → $20, caching $0.10 → $0.20); Gemini
        // 3.8 Flash $0.75 / $3.75 / $0.075 "through December 31, 2026", then
        // $1.50 / $7.50 / $0.15 "starting January 1, 2027". No time zone is
        // named: the change is at the UTC day boundary.
        let cost_on = |group: &str, model: &str, day: RateDay| {
            let t = TokenCounts {
                input: 1_000_000,
                output: 1_000_000,
                cache_read: Some(1_000_000),
                ..Default::default()
            };
            let class = PriceClass::STANDARD.resolve(group, model, &t, false, day);
            class_cost(group, model, class, &t, &empty()).expect("priced")
        };
        for model in [
            "gemini-4-argon",
            "argon",
            "gemini-4-argon[1m]",
            "gemini-4-argon-20261014",
        ] {
            approx(cost_on("gemini", model, NOV_11), 2.0 + 10.0 + 0.10);
            approx(
                cost_on("gemini", model, RateDay::ymd(2026, 11, 30)),
                2.0 + 10.0 + 0.10,
            );
            approx(
                cost_on("gemini", model, RateDay::ymd(2026, 12, 1)),
                4.0 + 20.0 + 0.20,
            );
        }
        for model in ["gemini-3.8-flash", "gemini-3.8-flash-high", "gemini"] {
            approx(
                cost_on("gemini", model, RateDay::ymd(2026, 12, 31)),
                0.75 + 3.75 + 0.075,
            );
            approx(
                cost_on("gemini", model, RateDay::ymd(2027, 1, 1)),
                1.50 + 7.50 + 0.15,
            );
        }
        // The gemini group fallback is Flash, dated changes included.
        approx(
            cost_on("gemini", "gemini-9-mystery", RateDay::ymd(2027, 1, 1)),
            1.50 + 7.50 + 0.15,
        );
        // The class records the change in force, so history keeps the rate
        // of its own day.
        let class = PriceClass::STANDARD.resolve(
            "gemini",
            "argon",
            &prompt(1, 0, 0),
            false,
            RateDay::ymd(2026, 12, 5),
        );
        assert_eq!(class.rates_from, Some(RateDay::ymd(2026, 12, 1)));
        assert_eq!(
            PriceClass::STANDARD
                .resolve("gemini", "argon", &prompt(1, 0, 0), false, NOV_11)
                .rates_from,
            None
        );
        // A row with no change never records one.
        assert_eq!(
            PriceClass::STANDARD
                .resolve(
                    "claude",
                    "claude-opus-5-5",
                    &prompt(1, 0, 0),
                    false,
                    RateDay::ymd(2027, 6, 1)
                )
                .rates_from,
            None
        );
    }

    #[test]
    fn retired_opus_4_1_and_opus_4_take_their_own_rates() {
        // platform.claude.com/docs/en/about-claude/pricing, read 2026-11-11:
        // Claude Opus 4.1 and Claude Opus 4 (retired) are $15 input / $18.75
        // 5m writes / $30 1h writes / $1.50 cache hits / $75 output. They fell
        // to the $5 Opus tier before.
        let want = ModelPrice::new(15.0, 75.0, 1.50, 18.75).with_extended(30.0);
        for model in [
            "claude-opus-4-1",
            "claude-opus-4-1-20250805",
            "claude-opus-4",
            "claude-opus-4-0",
            "claude-opus-4-20250514",
        ] {
            assert_eq!(price_for("claude", model, &empty()), Some(want), "{model}");
        }
        // The 4.5+ rows and their snapshots keep the Opus tier.
        for model in [
            "claude-opus-4-5",
            "claude-opus-4-5-20251101",
            "claude-opus-4-8",
            "claude-opus-4-10",
            "claude-opus-4-1x",
        ] {
            assert_eq!(
                price_for("claude", model, &empty()),
                Some(OPUS_TIER),
                "{model}"
            );
        }
    }

    #[test]
    fn an_aggregate_prices_as_the_sum_of_its_requests_per_class() {
        // Requests of one model in five classes; the aggregate keeps the
        // total plus the non-standard split.
        let requests = [
            (PriceClass::STANDARD, prompt(100_000, 50_000, 10_000)),
            (long(), prompt(250_000, 30_000, 5_000)),
            (fast(), prompt(1_000, 0, 0)),
            (
                PriceClass {
                    long_context: true,
                    fast: true,
                    ..PriceClass::STANDARD
                },
                prompt(300_000, 0, 0),
            ),
            (PriceClass::UNBILLED, prompt(400, 0, 0)),
        ];
        for (group, model) in [
            ("codex", "gpt-6.1-terra"),
            ("codex", "gpt-5.5"),
            ("grok", "grok-4.7"),
            ("claude", "claude-haiku-5-5"),
        ] {
            let mut total = TokenCounts::default();
            let mut split = ClassSplit::default();
            let mut sum = 0.0;
            for (class, t) in &requests {
                let t = TokenCounts { output: 777, ..*t };
                total = total.saturating_add(t);
                split.add(*class, &t);
                sum += class_cost(group, model, *class, &t, &empty()).expect("priced");
            }
            let aggregate = aggregate_cost(group, model, &total, &split, &empty()).expect("priced");
            assert!(
                (aggregate - sum).abs() < 1e-12,
                "{model}: {aggregate} != {sum}"
            );
            // Pricing the mixed total flat would understate it.
            assert!(priced_cost(group, model, &total, &empty()).expect("priced") < sum);
        }
        // No split: bit for bit the standard price of the total.
        let t = prompt(123_457, 5_555, 777);
        assert_eq!(
            aggregate_cost("codex", "gpt-6.1-sol", &t, &ClassSplit::default(), &empty())
                .map(f64::to_bits),
            priced_cost("codex", "gpt-6.1-sol", &t, &empty()).map(f64::to_bits)
        );
    }

    #[test]
    fn an_unbilled_refusal_costs_nothing_and_only_on_the_claude_group() {
        let t = prompt(4_000, 1_000, 0);
        assert_eq!(
            class_cost(
                "claude",
                "claude-fable-5-2",
                PriceClass::UNBILLED,
                &t,
                &empty()
            ),
            Some(0.0)
        );
        // Still `None` (no rate known) for an unknown model.
        assert_eq!(
            class_cost("nope", "nope", PriceClass::UNBILLED, &t, &empty()),
            None
        );
        let resolved =
            PriceClass::UNBILLED.resolve("claude", "claude-haiku-5-5", &t, false, NOV_11);
        assert_eq!(resolved, PriceClass::UNBILLED);
        // The rule is Anthropic's: another group's mark is ignored.
        assert!(
            !PriceClass::UNBILLED
                .resolve("gemini", "gemini-4-argon", &t, false, NOV_11)
                .unbilled
        );
    }

    #[test]
    fn an_override_prices_flat_unless_it_sets_its_own_tiers() {
        // An override replaces the whole row: without tiers a long or Fast
        // request prices at its standard rates, on every day.
        let flat: ModelPrice = serde_json::from_str(
            r#"{"input":1.0,"output":2.0,"cache_read":0.1,"cache_creation":1.25}"#,
        )
        .expect("parses");
        assert_eq!(
            serde_json::to_string(&flat).expect("serialize"),
            r#"{"input":1.0,"output":2.0,"cache_read":0.1,"cache_creation":1.25}"#
        );
        let both = PriceClass {
            long_context: true,
            fast: true,
            rates_from: Some(RateDay::ymd(2026, 12, 1)),
            unbilled: false,
        };
        assert_eq!(flat.rates(both), flat.standard());
        // With its own long rates, they apply to a request the BUILT-IN
        // boundary classifies long (the threshold is not configurable).
        let tiered: ModelPrice = serde_json::from_str(
            r#"{"input":1.0,"output":2.0,"cache_read":0.1,"cache_creation":1.25,
                "long_context":{"input":3.0,"output":4.0,"cache_read":0.3,"cache_creation":3.75},
                "fast":{"input":5.0,"output":6.0,"cache_read":0.5,"cache_creation":6.25}}"#,
        )
        .expect("parses");
        assert_eq!(tiered.long_boundary, None);
        let overrides = HashMap::from([("gpt-6.1-sol".to_string(), tiered)]);
        let t = prompt(300_000, 0, 0);
        let class = PriceClass::STANDARD.resolve("codex", "gpt-6.1-sol", &t, false, NOV_11);
        assert!(class.long_context);
        approx(
            class_cost("codex", "gpt-6.1-sol", class, &t, &overrides).expect("priced"),
            0.9,
        );
        approx(
            class_cost("codex", "gpt-6.1-sol", fast(), &t, &overrides).expect("priced"),
            1.5,
        );
        // Long and Fast with no `fast_long_context`: the long rates.
        approx(
            class_cost("codex", "gpt-6.1-sol", both, &t, &overrides).expect("priced"),
            0.9,
        );
        // A threshold cannot be configured.
        assert!(serde_json::from_str::<ModelPrice>(
            r#"{"input":1.0,"output":2.0,"cache_read":0.1,"cache_creation":1.25,
                "long_context":{"input":3.0,"output":4.0,"cache_read":0.3,"cache_creation":3.75,"threshold":1}}"#,
        )
        .is_err());
    }

    #[test]
    fn a_price_class_serializes_only_its_facts() {
        assert_eq!(
            serde_json::to_string(&PriceClass::STANDARD).expect("ser"),
            "{}"
        );
        let class = PriceClass {
            long_context: true,
            fast: true,
            rates_from: Some(RateDay::ymd(2026, 12, 1)),
            unbilled: false,
        };
        let json = serde_json::to_string(&class).expect("ser");
        assert_eq!(
            json,
            r#"{"long_context":true,"fast":true,"rates_from":"2026-12-01"}"#
        );
        assert_eq!(
            serde_json::from_str::<PriceClass>(&json).expect("de"),
            class
        );
        assert_eq!(
            serde_json::from_str::<PriceClass>("{}").expect("de"),
            PriceClass::STANDARD
        );
        assert!(serde_json::from_str::<PriceClass>(r#"{"rates_from":"2026-02-30"}"#).is_err());
        assert_eq!(RateDay::ymd(1970, 1, 1), RateDay(0));
        assert_eq!(RateDay::ymd(2026, 12, 1).to_string(), "2026-12-01");
    }
}
