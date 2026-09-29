//! API-equivalent USD pricing for token usage (Feature D).
//!
//! The dashboard tracks tokens per model and per request; this module turns
//! those token counts into the **API-equivalent USD cost** so the dashboard
//! can always show "$" alongside tokens. The proxy itself bills nothing — these
//! are *reference* prices: what the same traffic would cost on the provider's
//! pay-as-you-go API.
//!
//! ## The four-rate model
//! Anthropic and OpenAI both have cache tiers, but they price them differently:
//! Anthropic bills `cache_read` at a per-model fraction of input (0.1× on most
//! rows, less on Opus 5.5 and Fable 5.1) and `cache_creation` at 1.25× input;
//! OpenAI/codex bills cached input at a flat discounted rate and has no
//! cache-creation charge. A per-model [`ModelPrice`] with four independent
//! rates (input / output / cache_read / cache_creation) expresses both
//! providers uniformly — codex models simply carry `cache_creation: 0.0`.
//!
//! ## Long-context tier (per REQUEST, not per total)
//! Some providers bill a request at higher rates for ALL of its tokens when
//! that ONE request's prompt is large. A [`ModelPrice`] may carry
//! [`LongContextRates`]: a request whose prompt — fresh `input` plus
//! `cache_read` plus `cache_creation` (`input` is always FRESH input in
//! llmux) — is `>=` [`long_context_threshold`] is billed input, output, cache_read AND
//! cache_creation at the long rates (whole-request repricing, not a marginal
//! bracket). Explicit long rates per component, so a non-uniform tier (e.g. 2×
//! input/cache, 1.5× output) is plain data.
//!
//! Because the tier is a property of one request, a cost can NOT be derived
//! from aggregated token totals. Every aggregate keeps the long-context subset
//! separately ([`long_part`], folded per request) and prices through
//! [`aggregate_cost`]; single requests go through [`cost_usd`] /
//! [`request_breakdown`]. Both reduce to the same breakdown, so
//! **aggregate cost == sum of per-request costs** by construction. The split
//! point is model-keyed built-in data and deliberately independent of config
//! overrides — the activity fold and the SQLite query classify requests with
//! no config in hand.
//!
//! Tiered today: grok-4.5 / grok-4.6 / grok-4.7 (xAI, `>= 200k`, every rate
//! doubles; docs.x.ai read 2026-09-28) — and therefore the `grok` group
//! fallback — and the OpenAI rows gpt-5.5, gpt-5.6-{sol,terra,luna} and
//! gpt-6-{astra,sol,luna} (`>= 272k`, input and cache double, output x1.5; the
//! OpenAI pricing page, read 2026-09-28). The threshold is carried by the
//! built-in row ([`long_context_threshold`]), so a dated snapshot classifies
//! exactly as it prices. Deliberately NOT tiered: Claude rows (Claude 4.6+ bill
//! the full 1M window at standard rates) and any codex model the OpenAI page
//! does not list — `gpt-5.5-codex`, the `gpt-5.5-` prefix and the `codex`
//! group fallback stay flat. Enabling a tier on another row is a data-only
//! change: `.with_long_context_at(..)` plus an entry in `BUILTIN_TIERED_ROWS`.
//!
//! All rates are **USD per 1,000,000 tokens**. Rates sourced: every Claude row
//! from the Anthropic pricing page (platform.claude.com/docs/en/about-claude/
//! pricing), read 2026-09-28; OpenAI rows from the OpenAI pricing page, read
//! 2026-09-28; grok rows docs.x.ai 2026-09-28.

use std::collections::HashMap;

use crate::tui::activity::normalize_model;
use crate::tui::TokenCounts;

/// Per-model price table entry. All four rates are **USD per 1,000,000
/// tokens**. A zero rate means "free / not charged" (e.g. codex has no
/// cache-creation charge → `cache_creation: 0.0`).
///
/// Also the config `pricing` override shape. An override REPLACES the whole
/// entry, tier included: one without `long_context` prices every request at
/// its flat rates (a built-in tier is dropped, not inherited); one with
/// `long_context` applies those long rates at the model's built-in
/// [`long_context_threshold`] (the threshold is not configurable — see the
/// module docs). Config files written before the tier existed carry no
/// `long_context` key and load unchanged.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelPrice {
    /// Fresh (non-cached) input tokens, USD / 1e6.
    pub input: f64,
    /// Output (completion) tokens, USD / 1e6.
    pub output: f64,
    /// Cache-read tokens, USD / 1e6.
    pub cache_read: f64,
    /// Cache-creation (write) tokens, USD / 1e6.
    pub cache_creation: f64,
    /// Long-context tier: when set, a request whose prompt is `>=`
    /// [`long_context_threshold`] is billed ALL its tokens at these rates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_context: Option<LongContextRates>,
}

/// The rates a long-context request is billed at — every component, USD per
/// 1,000,000 tokens. Explicit rates rather than one multiplier, so a tier that
/// scales components differently is representable. `deny_unknown_fields`: a
/// config that tries to set e.g. a `threshold` here fails loudly instead of
/// being silently ignored.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LongContextRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_creation: f64,
    /// Prompt size at or above which the request is long-context. Built-in
    /// data (xAI 200k, OpenAI 272k): never read from or written to config —
    /// `skip` plus `deny_unknown_fields` makes a configured `threshold` fail
    /// loudly — and never taken from an override (see
    /// [`long_context_threshold`]).
    #[serde(skip, default = "default_long_context_threshold")]
    pub threshold: u64,
}

fn default_long_context_threshold() -> u64 {
    DEFAULT_LONG_CONTEXT_THRESHOLD
}

impl ModelPrice {
    const fn new(input: f64, output: f64, cache_read: f64, cache_creation: f64) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_creation,
            long_context: None,
        }
    }

    /// Attach a long-context tier (see [`LongContextRates`]).
    const fn with_long_context(
        self,
        input: f64,
        output: f64,
        cache_read: f64,
        cache_creation: f64,
    ) -> Self {
        self.with_long_context_at(
            DEFAULT_LONG_CONTEXT_THRESHOLD,
            input,
            output,
            cache_read,
            cache_creation,
        )
    }

    const fn with_long_context_at(
        self,
        threshold: u64,
        input: f64,
        output: f64,
        cache_read: f64,
        cache_creation: f64,
    ) -> Self {
        Self {
            long_context: Some(LongContextRates {
                input,
                output,
                cache_read,
                cache_creation,
                threshold,
            }),
            ..self
        }
    }

    /// All-zero rate — the fallback for genuinely unknown (group, model) pairs.
    /// Yields `0.0` cost and never panics.
    pub const fn zero() -> Self {
        Self::new(0.0, 0.0, 0.0, 0.0)
    }
}

/// The long-context boundary every tier modeled today uses (xAI: a prompt of
/// 200k tokens or more).
pub const DEFAULT_LONG_CONTEXT_THRESHOLD: u64 = 200_000;

/// OpenAI's long-context boundary (docs: prompts over 272k tokens).
pub const OPENAI_LONG_CONTEXT_THRESHOLD: u64 = 272_000;

/// Prompt size (fresh input + cache_read + cache_creation of ONE request) at
/// or above which `model` is billed at its long-context rates: the threshold
/// carried by the model's BUILT-IN price row (exact then prefix, the same
/// resolution as [`builtin_price`], so a dated snapshot classifies exactly as
/// it prices). A model with no tiered built-in row has no tier, so the value
/// is moot and the default is returned. Built-in data only — never a config
/// override — because the activity fold and the SQLite usage query classify
/// each request without a config handle, and every pricing path must agree
/// with that classification.
pub fn long_context_threshold(model: &str) -> u64 {
    let norm = normalize_model(model).to_ascii_lowercase();
    builtin_price(&norm)
        .and_then(|p| p.long_context)
        .map_or(DEFAULT_LONG_CONTEXT_THRESHOLD, |l| l.threshold)
}

/// Every distinct threshold [`long_context_threshold`] can return, ascending.
/// Lets a SQL `GROUP BY` bucket rows by prompt size exactly (the bucket index
/// is how many of these a row's prompt reaches) without knowing the model.
pub fn long_context_thresholds() -> Vec<u64> {
    let mut all: Vec<u64> = BUILTIN_TIERED_ROWS
        .iter()
        .filter_map(|p| p.long_context.map(|l| l.threshold))
        .collect();
    all.push(DEFAULT_LONG_CONTEXT_THRESHOLD);
    all.sort_unstable();
    all.dedup();
    all
}

/// Four token counters of one request, or of a SUM of requests. The pricing
/// input shape — absent (`None`) cache counters are `0` here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenParts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl TokenParts {
    /// The prompt the long-context boundary is measured on: fresh input plus
    /// both cache classes (llmux's `input` excludes cached tokens).
    pub fn prompt(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_creation)
    }

    /// Accumulate another request's (or bucket's) parts.
    pub fn add(&mut self, other: &TokenParts) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_creation = self.cache_creation.saturating_add(other.cache_creation);
    }

    fn saturating_sub(&self, other: &TokenParts) -> TokenParts {
        TokenParts {
            input: self.input.saturating_sub(other.input),
            output: self.output.saturating_sub(other.output),
            cache_read: self.cache_read.saturating_sub(other.cache_read),
            cache_creation: self.cache_creation.saturating_sub(other.cache_creation),
        }
    }
}

impl From<&TokenCounts> for TokenParts {
    fn from(t: &TokenCounts) -> Self {
        TokenParts {
            input: t.input,
            output: t.output,
            cache_read: t.cache_read.unwrap_or(0),
            cache_creation: t.cache_creation.unwrap_or(0),
        }
    }
}

/// The long-context share of ONE request: all of its parts when its prompt
/// reaches `model`'s [`long_context_threshold`], otherwise zero. Aggregates
/// fold this per request next to their totals and price with
/// [`aggregate_cost`] — the only way a tiered cost survives aggregation.
/// Classified for every model (not just tiered ones) so a config override
/// that adds a tier prices already-folded history correctly.
pub fn long_part(model: &str, tokens: &TokenCounts) -> TokenParts {
    let parts = TokenParts::from(tokens);
    if parts.prompt() >= long_context_threshold(model) {
        parts
    } else {
        TokenParts::default()
    }
}

/// Per-component API-equivalent cost. `total()` is exactly the sum of the
/// four, so a per-component display always adds up to the total shown.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CostBreakdown {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_creation: f64,
}

impl CostBreakdown {
    pub fn total(&self) -> f64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }
}

// ---- Claude rows ----
// Source for every Claude row: the Anthropic pricing page
// (platform.claude.com/docs/en/about-claude/pricing), read 2026-09-28. USD per
// 1M tokens, in the page's column order: base input / 5-minute cache write /
// 1-hour cache write / cache read / output. `cache_creation` here is the
// 5-minute write rate (1.25x input); the page's 1-hour write rate (2x input) is
// quoted alongside each row. No row carries a long-context tier (see the
// module docs): Claude 4.6+ bill the full 1M window at standard rates.

/// Claude Opus 5, 4.8, 4.7, 4.6, 4.5: 5 / 6.25 / 10 / 0.50 / 25. Also the
/// `group == "claude"` unknown-model fallback and the default for an Opus
/// version this table does not list.
const OPUS_TIER: ModelPrice = ModelPrice::new(5.0, 25.0, 0.5, 6.25);
/// Claude Opus 5.5: 4 / 5 / 8 / 0.20 / 20 — cheaper than the opus tier (cache
/// read is 0.05x input), so neither it nor a dated snapshot may fall to it.
const OPUS_5_5: ModelPrice = ModelPrice::new(4.0, 20.0, 0.20, 5.0);
/// Claude Opus 4.1 and Opus 4 (both retired): 15 / 18.75 / 30 / 1.50 / 75.
const OPUS_4_1: ModelPrice = ModelPrice::new(15.0, 75.0, 1.5, 18.75);
/// Claude Sonnet 5.5 and Sonnet 5: 2 / 2.50 / 4 / 0.20 / 10 (Sonnet 5's
/// $2/$10 is now its standard price, not a launch promotion).
const SONNET_5: ModelPrice = ModelPrice::new(2.0, 10.0, 0.2, 2.5);
/// Claude Sonnet 4.6, 4.5, 4: 3 / 3.75 / 6 / 0.30 / 15. Also the default for a
/// Sonnet version this table does not list.
const SONNET_TIER: ModelPrice = ModelPrice::new(3.0, 15.0, 0.3, 3.75);
/// Claude Haiku 4.5: 1 / 1.25 / 2 / 0.10 / 5. Also the default for a Haiku
/// version this table does not list.
const HAIKU_4_5: ModelPrice = ModelPrice::new(1.0, 5.0, 0.1, 1.25);
/// Claude Haiku 3.5 (retired; wire id `claude-3-5-haiku-*`): 0.80 / 1 / 1.60
/// / 0.08 / 4.
const HAIKU_3_5: ModelPrice = ModelPrice::new(0.8, 4.0, 0.08, 1.0);
/// Claude Fable 5: 10 / 12.50 / 20 / 1.00 / 50. Also the default for a Fable
/// version this table does not list.
const FABLE_5: ModelPrice = ModelPrice::new(10.0, 50.0, 1.0, 12.5);
/// Claude Fable 5.1: 10 / 12.50 / 20 / 0.25 / 50 — same as Fable 5 except the
/// cache read, which is 0.025x input ($0.25), not Fable 5's $1.00.
const FABLE_5_1: ModelPrice = ModelPrice::new(10.0, 50.0, 0.25, 12.5);
/// gpt-5.5 / codex default {input 5.0, output 30.0, cache_read 0.5,
/// cache_creation 0.0} (OpenAI pricing page, re-read 2026-09-28). Codex has no
/// cache-creation charge. Also the `group == "codex"` unknown-model fallback.
/// The page does not list `gpt-5.5-codex` / `gpt-5-codex`; they keep resolving
/// here by prefix / fallback, which is unverified.
const GPT_5_5: ModelPrice = ModelPrice::new(5.0, 30.0, 0.5, 0.0).with_long_context_at(
    OPENAI_LONG_CONTEXT_THRESHOLD,
    10.0,
    45.0,
    1.0,
    0.0,
);
/// The UNTIERED gpt-5.5 rates: the `group == "codex"` unknown-model fallback
/// and the `gpt-5.5-` prefix (`gpt-5.5-codex`, which the pricing page does not
/// list). Neither may claim a long-context tier nobody verified for them — and
/// an unknown model must classify flat under [`long_context_threshold`].
const GPT_5_5_FLAT: ModelPrice = ModelPrice::new(5.0, 30.0, 0.5, 0.0);
/// gpt-5.6-sol (flagship, 2026-07-09 launch, standard tier): $4 in / $20 out /
/// $0.40 cached input (OpenAI model page developers.openai.com/api/docs/models/
/// gpt-5.6-sol and the pricing page, read 2026-09-28). OpenAI calls this
/// promotional pricing "available at least through November 21, 2026", so
/// re-check it after that date. Codex: no cache-creation charge (the page's
/// $5 cache-write rate does not apply to subscription traffic). Prompts over
/// 272k bill the whole request at $8 in / $30 out / $0.80 cached.
const GPT_5_6_SOL: ModelPrice = ModelPrice::new(4.0, 20.0, 0.4, 0.0).with_long_context_at(
    OPENAI_LONG_CONTEXT_THRESHOLD,
    8.0,
    30.0,
    0.8,
    0.0,
);
/// gpt-5.6-terra (mid tier): $2 in / $12 out / $0.20 cached input (OpenAI
/// pricing page, read 2026-09-28; same promotional caveat and conventions as
/// [`GPT_5_6_SOL`]).
const GPT_5_6_TERRA: ModelPrice = ModelPrice::new(2.0, 12.0, 0.2, 0.0).with_long_context_at(
    OPENAI_LONG_CONTEXT_THRESHOLD,
    4.0,
    18.0,
    0.4,
    0.0,
);
/// gpt-5.6-luna (budget tier): $0.20 in / $1.20 out / $0.02 cached input
/// (OpenAI pricing page, read 2026-09-28; same caveats as [`GPT_5_6_SOL`]).
const GPT_5_6_LUNA: ModelPrice = ModelPrice::new(0.2, 1.2, 0.02, 0.0).with_long_context_at(
    OPENAI_LONG_CONTEXT_THRESHOLD,
    0.4,
    1.8,
    0.04,
    0.0,
);
/// gpt-6-astra (generation-6 flagship, 2026-09 launch, standard tier): $10 in
/// / $50 out / $1 cached input (OpenAI API pricing page, read 2026-09-28).
/// Codex: no cache-creation charge, same convention as the other codex rows.
const GPT_6_ASTRA: ModelPrice = ModelPrice::new(10.0, 50.0, 1.0, 0.0).with_long_context_at(
    OPENAI_LONG_CONTEXT_THRESHOLD,
    20.0,
    75.0,
    2.0,
    0.0,
);
/// gpt-6-sol (2026-09-22 launch, standard tier, prompts <=272k): $2 in / $10
/// out / $0.20 cached input (OpenAI API pricing page,
/// developers.openai.com/api/docs/pricing, read 2026-09-28). Codex: no
/// cache-creation charge, same convention as the other codex rows (the page's
/// separate $2.50 cache-write rate does not apply to subscription traffic).
/// Prompts over 272k bill the whole request at $4 in / $15 out / $0.40 cached.
const GPT_6_SOL: ModelPrice = ModelPrice::new(2.0, 10.0, 0.2, 0.0).with_long_context_at(
    OPENAI_LONG_CONTEXT_THRESHOLD,
    4.0,
    15.0,
    0.4,
    0.0,
);
/// gpt-6-luna (2026-09-22 launch, standard tier): $0.10 in / $0.50 out /
/// $0.01 cached input; over 272k: $0.20 in / $0.75 out / $0.02 cached.
/// Same sourcing and conventions as [`GPT_6_SOL`].
const GPT_6_LUNA: ModelPrice = ModelPrice::new(0.1, 0.5, 0.01, 0.0).with_long_context_at(
    OPENAI_LONG_CONTEXT_THRESHOLD,
    0.2,
    0.75,
    0.02,
    0.0,
);
/// grok-4.5 (docs.x.ai, read 2026-09-28): $2 in / $6 out / $0.30 cached input
/// (the row previously carried $0.50 — the page lists $0.30), no
/// cache-creation charge; long context (prompt >= 200k): $4 / $12 / $0.60 for
/// the whole request. Also the `group == "grok"` unknown-model fallback.
/// Like the codex rows, an API-list-price EQUIVALENT for subscription
/// traffic, not a billed amount (docs/grok/spec.md §Compatibility).
const GROK_4_5: ModelPrice =
    ModelPrice::new(2.0, 6.0, 0.3, 0.0).with_long_context(4.0, 12.0, 0.6, 0.0);
/// grok-4.6 (docs.x.ai, read 2026-09-28): $2 in / $6 out / $0.50 cached input;
/// long context (prompt >= 200k): $4 / $12 / $1.00 for the whole request.
/// API-list-price equivalent for subscription traffic, like the other grok
/// rows.
const GROK_4_6: ModelPrice =
    ModelPrice::new(2.0, 6.0, 0.5, 0.0).with_long_context(4.0, 12.0, 1.0, 0.0);
/// grok-4.7 (docs.x.ai, read 2026-09-28): $2 in / $6 out / $0.50 cached input,
/// no cache-creation charge — unchanged from grok-4.6, as is its long-context
/// tier (prompt >= 200k): $4 / $12 / $1.00 for the whole request.
/// API-list-price equivalent for subscription traffic.
const GROK_4_7: ModelPrice =
    ModelPrice::new(2.0, 6.0, 0.5, 0.0).with_long_context(4.0, 12.0, 1.0, 0.0);
// No long-context tier on the Claude rows: Anthropic bills Claude 4.6+ at
// standard rates across the full 1M window (pricing page, read 2026-09-28).

/// Every built-in row that carries a long-context tier — the source of
/// [`long_context_thresholds`]. Adding a tiered row means listing it here (a
/// test fails when a resolvable tiered row's threshold is missing).
const BUILTIN_TIERED_ROWS: &[ModelPrice] = &[
    GPT_5_5,
    GPT_5_6_SOL,
    GPT_5_6_TERRA,
    GPT_5_6_LUNA,
    GPT_6_ASTRA,
    GPT_6_SOL,
    GPT_6_LUNA,
    GROK_4_5,
    GROK_4_6,
    GROK_4_7,
];
/// Free — all four rates zero. Applied to the CURATED OpenRouter set, every
/// member of which had `pricing.prompt == "0"` and `pricing.completion == "0"`
/// on the live `GET /api/v1/models` probe of 2026-08-21
/// (docs/openrouter/spec.md). Deliberately NOT the `group == "openrouter"`
/// fallback: OpenRouter also serves ~400 PAID models reachable through the
/// `or-<vendor>/<slug>` escape hatch, and asserting $0 for those would be an
/// invented number.
const OPENROUTER_FREE: ModelPrice = ModelPrice::new(0.0, 0.0, 0.0, 0.0);

/// Look up the built-in default price for a *normalized*, lowercased model
/// slug. Claude ids go through a version parser ([`claude_price`], so e.g.
/// `claude-opus-4-8-20260101` resolves to the opus tier); every other id takes
/// exact matches first, then a prefix fallback. Returns `None`
/// when nothing matches — callers apply the group fallback.
fn builtin_price(model_norm_lower: &str) -> Option<ModelPrice> {
    // Claude ids carry their version as dash-separated segments, where a
    // dated snapshot (`claude-opus-4-20250514`) and a minor version
    // (`claude-opus-4-5`) share one separator — so a prefix chain cannot tell
    // `claude-opus-4-` (Opus 4, $15) from `claude-opus-4-5-` (Opus 4.5, $5).
    // Parse the version instead; see [`claude_price`].
    if let Some(p) = claude_price(model_norm_lower) {
        return Some(p);
    }
    // Exact (post-normalization) matches.
    let exact = match model_norm_lower {
        "gpt-5.5" => Some(GPT_5_5),
        "gpt-5.6" | "gpt-5.6-sol" => Some(GPT_5_6_SOL),
        "gpt-5.6-terra" => Some(GPT_5_6_TERRA),
        "gpt-5.6-luna" => Some(GPT_5_6_LUNA),
        "gpt-6" | "gpt-6-astra" => Some(GPT_6_ASTRA),
        "gpt-6-sol" => Some(GPT_6_SOL),
        "gpt-6-luna" => Some(GPT_6_LUNA),
        "grok-4.5" => Some(GROK_4_5),
        "grok-4.7" => Some(GROK_4_7),
        "grok-4.6" => Some(GROK_4_6),
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
    if model_norm_lower.starts_with("gpt-5.5-") {
        // Generation boundary: bare `gpt-5.5` matched exactly above; the
        // prefix branch requires the `-` so `gpt-5.50-*` never takes 5.5
        // rates (mirrors codex.rs `supports_extended_efforts`).
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
    } else if model_norm_lower.starts_with("gpt-6-astra-") {
        Some(GPT_6_ASTRA)
    } else if model_norm_lower.starts_with("gpt-6-sol-") {
        Some(GPT_6_SOL)
    } else if model_norm_lower.starts_with("gpt-6-luna-") {
        Some(GPT_6_LUNA)
    } else if model_norm_lower.starts_with("gpt-6-") {
        // Astra is generation 6's flagship (and its only tier), so it is the
        // `gpt-6-` default exactly as sol is for `gpt-5.6-`. The required `-`
        // is the generation boundary: `gpt-60-*` and `gpt-6.5-*` are DIFFERENT
        // generations and must miss this branch (mirrors codex.rs
        // `supports_extended_efforts`).
        Some(GPT_6_ASTRA)
    } else {
        None
    }
}

/// A Claude model id split into family and version (`major`, optional
/// `minor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClaudeId<'a> {
    family: &'a str,
    /// `None` when the segments after the family do not parse as a version
    /// (a stray suffix, a trailing `-`): the id is a known family at an
    /// unknown version.
    version: Option<(u32, Option<u32>)>,
    /// The Claude 3.x naming scheme (`claude-3-5-haiku-*`), version first.
    legacy: bool,
}

const CLAUDE_FAMILIES: &[&str] = &["opus", "sonnet", "haiku", "fable"];

/// Parse a normalized, lowercased Claude id in either naming scheme:
/// `claude-<family>-<major>[-<minor>][-<snapshot>]` (Claude 4 onward) or
/// `claude-<major>[-<minor>]-<family>[-<snapshot>]` (Claude 3.x). A version
/// segment is 1-2 digits and a snapshot is an 8-digit date or `latest`, which
/// is what separates `claude-opus-4-20250514` (Opus 4, dated) from
/// `claude-opus-4-5` (Opus 4.5) — both are `claude-opus-4-` + a digit run.
/// Anything else (`claude-future-9`, an unlisted family) is `None`.
fn parse_claude_id(slug: &str) -> Option<ClaudeId<'_>> {
    let segs: Vec<&str> = slug.strip_prefix("claude-")?.split('-').collect();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let is_ver = |s: &str| s.len() <= 2 && digits(s);
    let is_snapshot = |s: &str| s == "latest" || (s.len() == 8 && digits(s));
    let num = |s: &str| s.parse::<u32>().ok();
    // `[major, minor?]` exactly.
    let version = |v: &[&str]| -> Option<(u32, Option<u32>)> {
        match v {
            [major] if is_ver(major) => Some((num(major)?, None)),
            [major, minor] if is_ver(major) && is_ver(minor) => {
                Some((num(major)?, Some(num(minor)?)))
            }
            _ => None,
        }
    };
    // Strip at most one trailing snapshot segment.
    let unsnapshot = |v: &[&str]| -> usize {
        match v.last() {
            Some(s) if is_snapshot(s) => v.len() - 1,
            _ => v.len(),
        }
    };
    if CLAUDE_FAMILIES.contains(segs.first()?) {
        let rest = &segs[1..];
        return Some(ClaudeId {
            family: segs[0],
            version: version(&rest[..unsnapshot(rest)]),
            legacy: false,
        });
    }
    // Claude 3.x: version, then family, then an optional snapshot — nothing
    // else. A partial parse is not a Claude 3.x id we can price.
    let at = segs.iter().position(|s| CLAUDE_FAMILIES.contains(s))?;
    let tail = &segs[at + 1..];
    if unsnapshot(tail) != 0 {
        return None;
    }
    Some(ClaudeId {
        family: segs[at],
        version: Some(version(&segs[..at])?),
        legacy: true,
    })
}

/// The built-in row for a Claude id (see the Claude row constants for the
/// source). Listed versions take their own row. An UNLISTED version of a known
/// family (`claude-opus-5-50-*`, `claude-sonnet-6`, `claude-opus-5-5x`) takes
/// the family's default row — the same rates the old `claude-<family>-`
/// prefix fallback charged. Unlisted Claude 3.x ids return `None` and take the
/// `group == "claude"` fallback, as they did before this parser existed.
fn claude_price(slug: &str) -> Option<ModelPrice> {
    let id = parse_claude_id(slug)?;
    if id.legacy {
        return match (id.family, id.version) {
            ("haiku", Some((3, Some(5)))) => Some(HAIKU_3_5),
            _ => None,
        };
    }
    Some(match (id.family, id.version) {
        ("opus", Some((5, Some(5)))) => OPUS_5_5,
        ("opus", Some((5, None) | (4, Some(5..=8)))) => OPUS_TIER,
        // `claude-opus-4` / `-4-0` / `-4-<date>` is Opus 4; `-4-1` is Opus 4.1.
        ("opus", Some((4, None | Some(0 | 1)))) => OPUS_4_1,
        ("opus", _) => OPUS_TIER,
        ("sonnet", Some((5, None | Some(5)))) => SONNET_5,
        ("sonnet", _) => SONNET_TIER,
        ("haiku", _) => HAIKU_4_5,
        ("fable", Some((5, Some(1)))) => FABLE_5_1,
        ("fable", _) => FABLE_5,
        _ => return None,
    })
}

/// Resolve the price for `(group, model)`, honoring config `overrides` first.
///
/// Resolution order:
/// 1. `overrides` keyed by the **normalized** model (case preserved as written
///    in config, but matched case-insensitively against the normalized model).
/// 2. The built-in default table (exact then prefix), case-insensitive.
/// 3. Group fallback: `group == "claude"` → opus-tier rates; `group ==
///    "codex"` → gpt-5.5 rates; any other group → `None` (all-zero cost).
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
        _ => None,
    }
}

/// API-equivalent USD cost of ONE request under `(group, model)`'s price,
/// long-context tier included. Unknown / zero-rate model → `0.0`. Never
/// panics. Absent (`None`) cache fields contribute `0`.
///
/// Per-request only: `tokens` must be a single request's usage, because the
/// tier is decided on its prompt size. Aggregates use [`aggregate_cost`].
pub fn cost_usd(
    group: &str,
    model: &str,
    tokens: &TokenCounts,
    overrides: &HashMap<String, ModelPrice>,
) -> f64 {
    request_breakdown(group, model, tokens, overrides).map_or(0.0, |b| b.total())
}

/// Per-component cost of ONE request (the TUI's expanded-row breakdown).
/// `None` means "no rate known". The whole request is classified once, so a
/// long-context request prices every component at the long rates and the
/// four components still sum to [`cost_usd`].
pub fn request_breakdown(
    group: &str,
    model: &str,
    tokens: &TokenCounts,
    overrides: &HashMap<String, ModelPrice>,
) -> Option<CostBreakdown> {
    let all = TokenParts::from(tokens);
    let long = long_part(model, tokens);
    Some(breakdown(price_for(group, model, overrides)?, &all, &long))
}

/// Cost of an AGGREGATE of requests: `all` is every request's parts summed,
/// `long` the subset from requests whose own prompt reached the threshold
/// (summed [`long_part`]s). The short remainder is billed at the flat rates,
/// the long subset at the tier rates, so the result equals the sum of the
/// per-request [`cost_usd`]s. A model with no tier ignores `long` and prices
/// `all` exactly as before tiering existed.
///
/// `None` means "no rate known for this `(group, model)`", so a caller can
/// never mistake a missing rate for a free row (usage-stats review — the
/// `priced` flag and the cost must come from ONE lookup).
pub fn aggregate_cost(
    group: &str,
    model: &str,
    all: &TokenParts,
    long: &TokenParts,
    overrides: &HashMap<String, ModelPrice>,
) -> Option<f64> {
    Some(breakdown(price_for(group, model, overrides)?, all, long).total())
}

/// The single pricing formula every entry point reduces to:
/// `component = short·flat/1e6 + long·tier/1e6` per component. Without a tier
/// the long subset is not separated at all — `all·flat/1e6`, bit-identical to
/// the pre-tier formula.
fn breakdown(price: ModelPrice, all: &TokenParts, long: &TokenParts) -> CostBreakdown {
    let per_m = |count: u64, rate: f64| (count as f64) * rate / 1_000_000.0;
    match price.long_context {
        None => CostBreakdown {
            input: per_m(all.input, price.input),
            output: per_m(all.output, price.output),
            cache_read: per_m(all.cache_read, price.cache_read),
            cache_creation: per_m(all.cache_creation, price.cache_creation),
        },
        Some(tier) => {
            let short = all.saturating_sub(long);
            CostBreakdown {
                input: per_m(short.input, price.input) + per_m(long.input, tier.input),
                output: per_m(short.output, price.output) + per_m(long.output, tier.output),
                cache_read: per_m(short.cache_read, price.cache_read)
                    + per_m(long.cache_read, tier.cache_read),
                cache_creation: per_m(short.cache_creation, price.cache_creation)
                    + per_m(long.cache_creation, tier.cache_creation),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Short-context (sub-threshold) cost of ONE token component at the
    /// per-1M rate: prices 100k tokens and scales, because a single 1M-token
    /// request is itself long-context on every tiered model.
    fn per_m(group: &str, model: &str, component: usize) -> f64 {
        let n = 100_000;
        let t = match component {
            0 => tc(n, 0, None, None),
            1 => tc(0, n, None, None),
            2 => tc(0, 0, Some(n), None),
            _ => tc(0, 0, None, Some(n)),
        };
        cost_usd(group, model, &t, &empty()) * 10.0
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
        // grok-4.5 cached input is $0.30 (docs.x.ai, read 2026-09-28).
        let p = price_for("grok", "grok-4.5", &overrides).expect("grok-4.5 priced");
        assert_eq!(
            (p.input, p.output, p.cache_read, p.cache_creation),
            (2.0, 6.0, 0.3, 0.0)
        );
        let p6 = price_for("grok", "grok-4.6", &overrides).expect("grok-4.6 priced");
        assert_eq!(
            (p6.input, p6.output, p6.cache_read, p6.cache_creation),
            (2.0, 6.0, 0.5, 0.0)
        );
        // grok-4.7: same flat rates as 4.6 (its long-context tier is
        // covered by the tier tests below).
        let p7 = price_for("grok", "grok-4.7", &overrides).expect("grok-4.7 priced");
        assert_eq!(
            (p7.input, p7.output, p7.cache_read, p7.cache_creation),
            (2.0, 6.0, 0.5, 0.0)
        );
        let f = price_for("grok", "grok-build-0.1", &overrides).expect("group fallback");
        assert_eq!(
            (f.input, f.output),
            (2.0, 6.0),
            "unknown grok model → grok fallback"
        );
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
        let cost = per_m("codex", "gpt-5.5", 1);
        approx(cost, 30.00);
    }

    #[test]
    fn gpt_5_5_has_no_cache_creation_charge() {
        // Codex never bills cache creation; even a huge count costs nothing for it.
        let cost = per_m("codex", "gpt-5.5", 3);
        approx(cost, 0.0);
    }

    #[test]
    fn gpt_5_6_sol_matches_exact_and_bare_and_prefix() {
        // Exact `gpt-5.6-sol`, the bare `gpt-5.6` alias, and a future dated
        // snapshot all resolve to sol rates ($4 in / $20 out / $0.40 cache read).
        for model in ["gpt-5.6-sol", "gpt-5.6", "gpt-5.6-sol-20260709"] {
            let cost = per_m("codex", model, 0);
            approx(cost, 4.00);
            let out = per_m("codex", model, 1);
            approx(out, 20.00);
            let cached = per_m("codex", model, 2);
            approx(cached, 0.40);
        }
    }

    #[test]
    fn gpt_6_astra_matches_exact_and_bare_and_prefix() {
        // Exact `gpt-6-astra`, the bare `gpt-6` alias, and a dated snapshot
        // all resolve to astra rates ($10 in / $50 out / $1 cached input).
        for model in ["gpt-6-astra", "gpt-6", "gpt-6-astra-20260903"] {
            let cost = per_m("codex", model, 0);
            approx(cost, 10.00);
            let out = per_m("codex", model, 1);
            approx(out, 50.00);
            let cached = per_m("codex", model, 2);
            approx(cached, 1.00);
        }
        // Codex convention: no cache-creation charge.
        let creation = per_m("codex", "gpt-6-astra", 3);
        approx(creation, 0.0);
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
    fn gpt_6_sol_and_luna_have_their_own_rates() {
        // Exact slugs and dated snapshots take the sol / luna rows, NOT the
        // `gpt-6-` astra default ($10 / $50 / $1).
        for (model, input, output, cached) in [
            ("gpt-6-sol", 2.00, 10.00, 0.20),
            ("gpt-6-sol-20260922", 2.00, 10.00, 0.20),
            ("gpt-6-luna", 0.10, 0.50, 0.01),
            ("gpt-6-luna-20260922", 0.10, 0.50, 0.01),
        ] {
            approx(per_m("codex", model, 0), input);
            approx(per_m("codex", model, 1), output);
            approx(per_m("codex", model, 2), cached);
            // Codex convention: no cache-creation charge.
            approx(per_m("codex", model, 3), 0.0);
        }
        // A future gpt-6 tier we have no row for still takes the astra default.
        assert_eq!(builtin_price("gpt-6-terra"), Some(GPT_6_ASTRA));
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
        // (model, input, output, cached) per 1M, OpenAI pricing page.
        for (model, input, output, cached) in [
            ("gpt-5.6-terra", 2.00, 12.00, 0.20),
            ("gpt-5.6-luna", 0.20, 1.20, 0.02),
        ] {
            approx(per_m("codex", model, 0), input);
            approx(per_m("codex", model, 1), output);
            approx(per_m("codex", model, 2), cached);
        }
    }

    #[test]
    fn gpt_5_6_has_no_cache_creation_charge() {
        let cost = per_m("codex", "gpt-5.6-sol", 3);
        approx(cost, 0.0);
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
        // input rate). gpt-5.6-sol input is $4/1e6, so 1e6 cache-read tokens
        // cost $0.40 — a third of a mostly-cached prompt is billed at a tenth,
        // not the full input rate (the codex cache-read cost regression).
        let cache = per_m("codex", "gpt-5.6-sol", 2);
        approx(cache, 0.40);
        let input = per_m("codex", "gpt-5.6-sol", 0);
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

    /// (input, output, cache_read, cache_creation) of a resolved Claude row.
    fn claude_rates(model: &str) -> (f64, f64, f64, f64) {
        let p = price_for("claude", model, &empty()).expect("claude priced");
        assert!(
            p.long_context.is_none(),
            "{model}: Claude rows are untiered"
        );
        (p.input, p.output, p.cache_read, p.cache_creation)
    }

    /// Every Claude row family against the Anthropic pricing page (read
    /// 2026-09-28), bare id and the `[1m]` display spelling alike.
    #[test]
    fn claude_rows_match_the_pricing_page() {
        let opus = (5.0, 25.0, 0.5, 6.25);
        let opus_legacy = (15.0, 75.0, 1.5, 18.75);
        let sonnet_5 = (2.0, 10.0, 0.2, 2.5);
        let sonnet_4 = (3.0, 15.0, 0.3, 3.75);
        for (model, want) in [
            ("claude-fable-5-1", (10.0, 50.0, 0.25, 12.5)),
            ("claude-fable-5", (10.0, 50.0, 1.0, 12.5)),
            ("claude-opus-5-5", (4.0, 20.0, 0.2, 5.0)),
            ("claude-opus-5", opus),
            ("claude-opus-4-8", opus),
            ("claude-opus-4-7", opus),
            ("claude-opus-4-6", opus),
            ("claude-opus-4-5", opus),
            ("claude-opus-4-1", opus_legacy),
            ("claude-opus-4", opus_legacy),
            ("claude-opus-4-0", opus_legacy),
            ("claude-sonnet-5-5", sonnet_5),
            ("claude-sonnet-5", sonnet_5),
            ("claude-sonnet-4-6", sonnet_4),
            ("claude-sonnet-4-5", sonnet_4),
            ("claude-sonnet-4", sonnet_4),
            ("claude-sonnet-4-0", sonnet_4),
            ("claude-haiku-4-5", (1.0, 5.0, 0.1, 1.25)),
            ("claude-3-5-haiku-20241022", (0.8, 4.0, 0.08, 1.0)),
        ] {
            assert_eq!(claude_rates(model), want, "{model}");
            assert_eq!(claude_rates(&format!("{model}[1m]")), want, "{model}[1m]");
        }
        // The curated aliases resolve through `normalize_model` first.
        assert_eq!(claude_rates("sonnet"), sonnet_5, "sonnet → claude-sonnet-5");
        assert_eq!(claude_rates("fable").2, 0.25, "fable → claude-fable-5-1");
        assert_eq!(claude_rates("haiku"), (1.0, 5.0, 0.1, 1.25));
    }

    /// Dated snapshots price exactly like their bare id — including the
    /// version-vs-date ambiguity: `claude-opus-4-20250514` is Opus 4 ($15),
    /// `claude-opus-4-5-20251101` is Opus 4.5 ($5), and
    /// `claude-sonnet-5-5-*` is Sonnet 5.5, never a shorter `claude-sonnet-`
    /// match.
    #[test]
    fn claude_dated_snapshots_price_like_their_bare_id() {
        for (snapshot, bare) in [
            ("claude-fable-5-1-20260901", "claude-fable-5-1"),
            ("claude-fable-5-20260601", "claude-fable-5"),
            ("claude-opus-5-5-20260922", "claude-opus-5-5"),
            ("claude-opus-5-20260727", "claude-opus-5"),
            ("claude-opus-4-8-20260101", "claude-opus-4-8"),
            ("claude-opus-4-5-20251101", "claude-opus-4-5"),
            ("claude-opus-4-1-20250805", "claude-opus-4-1"),
            ("claude-opus-4-20250514", "claude-opus-4"),
            ("claude-sonnet-5-5-20261001", "claude-sonnet-5-5"),
            ("claude-sonnet-5-20260801", "claude-sonnet-5"),
            ("claude-sonnet-4-5-20250929", "claude-sonnet-4-5"),
            ("claude-sonnet-4-20250514", "claude-sonnet-4"),
            ("claude-haiku-4-5-20251001", "claude-haiku-4-5"),
            ("claude-3-5-haiku-latest", "claude-3-5-haiku-20241022"),
        ] {
            assert_eq!(claude_rates(snapshot), claude_rates(bare), "{snapshot}");
        }
    }

    /// Version boundaries: an id that only LOOKS like a listed version is a
    /// different model and takes its family default (the rates the old
    /// `claude-<family>-` prefix fallback charged), never the listed row.
    #[test]
    fn claude_version_boundaries_fall_to_the_family_default() {
        let opus = (5.0, 25.0, 0.5, 6.25);
        let sonnet_4 = (3.0, 15.0, 0.3, 3.75);
        for (model, want) in [
            ("claude-sonnet-5-50-20261001", sonnet_4),
            ("claude-sonnet-5-5x", sonnet_4),
            ("claude-sonnet-55", sonnet_4),
            ("claude-opus-4-10", opus),
            ("claude-opus-4-1x", opus),
            ("claude-fable-5-10", (10.0, 50.0, 1.0, 12.5)),
            ("claude-fable-5-1-preview", (10.0, 50.0, 1.0, 12.5)),
            ("claude-haiku-5", (1.0, 5.0, 0.1, 1.25)),
        ] {
            assert_eq!(claude_rates(model), want, "{model}");
        }
        // Unlisted families and Claude 3.x ids keep the group fallback.
        for model in [
            "claude-mythos-5",
            "claude-3-7-sonnet-20250219",
            "claude-3-haiku",
        ] {
            assert_eq!(claude_rates(model), opus, "{model}");
            assert!(builtin_price(model).is_none(), "{model}");
        }
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
        let cost = per_m("codex", "gpt-7-mini", 1);
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
    fn aggregate_cost_of_one_request_matches_cost_usd() {
        let tokens = tc(700, 300, Some(120), None);
        let a = cost_usd("codex", "gpt-5.5", &tokens, &empty());
        let b = aggregate_cost(
            "codex",
            "gpt-5.5",
            &TokenParts::from(&tokens),
            &long_part("gpt-5.5", &tokens),
            &empty(),
        )
        .expect("priced");
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

    // ---- long-context tier (docs.x.ai, read 2026-09-28) ----

    /// Old per-component formula, spelled out: the exact flat-rate cost the
    /// module computed before tiering existed.
    fn flat_formula(p: ModelPrice, t: &TokenCounts) -> f64 {
        let per_m = |count: u64, rate: f64| (count as f64) * rate / 1_000_000.0;
        per_m(t.input, p.input)
            + per_m(t.output, p.output)
            + per_m(t.cache_read.unwrap_or(0), p.cache_read)
            + per_m(t.cache_creation.unwrap_or(0), p.cache_creation)
    }

    #[test]
    fn grok_tier_boundary_is_inclusive_at_200k_prompt_tokens() {
        // Prompt = fresh input + cache_read + cache_creation.
        let short = tc(100_000, 1_000, Some(99_999), None); // 199,999
        let long = tc(100_000, 1_000, Some(100_000), None); // exactly 200,000
        approx(
            cost_usd("grok", "grok-4.7", &short, &empty()),
            (100_000.0 * 2.0 + 1_000.0 * 6.0 + 99_999.0 * 0.5) / 1e6,
        );
        approx(
            cost_usd("grok", "grok-4.7", &long, &empty()),
            (100_000.0 * 4.0 + 1_000.0 * 12.0 + 100_000.0 * 1.0) / 1e6,
        );
        // cache_creation counts toward the prompt too.
        assert_eq!(
            long_part("grok-4.7", &tc(1, 0, None, Some(199_999))),
            TokenParts {
                input: 1,
                output: 0,
                cache_read: 0,
                cache_creation: 199_999
            }
        );
        // Output never counts toward the prompt.
        assert_eq!(
            long_part("grok-4.7", &tc(199_999, 1_000_000, None, None)),
            TokenParts::default()
        );
    }

    #[test]
    fn grok_long_context_rates_per_model() {
        // (model, long input, long output, long cached) per docs.x.ai.
        for (model, li, lo, lc) in [
            ("grok-4.7", 4.0, 12.0, 1.0),
            ("grok-4.6", 4.0, 12.0, 1.0),
            ("grok-4.5", 4.0, 12.0, 0.6),
            // Unknown grok model → grok-4.5 group fallback, tier included.
            ("grok-build-0.1", 4.0, 12.0, 0.6),
        ] {
            let b = request_breakdown(
                "grok",
                model,
                &tc(300_000, 1_000_000, Some(1_000_000), None),
                &empty(),
            )
            .expect("priced");
            approx(b.input, 0.3 * li);
            approx(b.output, lo);
            approx(b.cache_read, lc);
        }
    }

    #[test]
    fn a_long_request_reprices_every_component_including_cache_creation() {
        // Distinct rates everywhere so a component billed at the wrong tier
        // cannot hide; grok itself has no cache-write charge, so exercise the
        // cache_creation leg through an override tier.
        let mut overrides = HashMap::new();
        overrides.insert(
            "grok-4.7".to_string(),
            ModelPrice::new(1.0, 2.0, 3.0, 4.0).with_long_context(10.0, 20.0, 30.0, 40.0),
        );
        let t = tc(100_000, 50_000, Some(60_000), Some(40_000)); // prompt 200k
        let b = request_breakdown("grok", "grok-4.7", &t, &overrides).expect("priced");
        approx(b.input, 100_000.0 * 10.0 / 1e6);
        approx(b.output, 50_000.0 * 20.0 / 1e6);
        approx(b.cache_read, 60_000.0 * 30.0 / 1e6);
        approx(b.cache_creation, 40_000.0 * 40.0 / 1e6);
        // The TUI split adds up to the total exactly.
        assert_eq!(b.total(), cost_usd("grok", "grok-4.7", &t, &overrides));
        assert_eq!(
            b.input + b.output + b.cache_read + b.cache_creation,
            b.total()
        );
    }

    #[test]
    fn aggregate_cost_equals_the_sum_of_per_request_costs() {
        // The bug class: one long request priced into a mixed total at flat
        // rates. Aggregates must keep the long subset separate.
        let requests = [
            tc(1_000, 500, Some(150_000), None),    // short
            tc(50_000, 2_000, Some(150_000), None), // long (== 200k)
            tc(250_000, 1_000, None, Some(0)),      // long
            tc(199_999, 10, None, None),            // short (boundary - 1)
            tc(10, 20, None, None),                 // short
        ];
        for model in [
            "grok-4.7",
            "grok-4.6",
            "grok-4.5",
            "claude-opus-4-8",
            "gpt-5.5",
        ] {
            let group = if model.starts_with("grok") {
                "grok"
            } else if model.starts_with("claude") {
                "claude"
            } else {
                "codex"
            };
            let (mut all, mut long) = (TokenParts::default(), TokenParts::default());
            let mut sum = 0.0;
            for t in &requests {
                all.add(&TokenParts::from(t));
                long.add(&long_part(model, t));
                sum += cost_usd(group, model, t, &empty());
            }
            let agg = aggregate_cost(group, model, &all, &long, &empty()).expect("priced");
            assert!(
                (agg - sum).abs() < 1e-12,
                "{model}: aggregate {agg} != sum {sum}"
            );
            if group == "grok" {
                // Non-vacuous: pricing the mixed total flat undercounts.
                let flat = aggregate_cost(group, model, &all, &TokenParts::default(), &empty())
                    .expect("priced");
                assert!(agg > flat + 1e-6, "{model}: long requests must cost more");
            }
        }
    }

    #[test]
    fn untiered_models_are_bit_for_bit_unchanged() {
        // Claude rows and unverified codex models carry no tier: even a > 200k request, and an
        // aggregate with a non-empty long subset, price at the flat formula
        // with identical floating-point results.
        let big = tc(180_000, 7_777, Some(123_456), Some(54_321));
        let small = tc(700, 300, Some(120), None);
        for (group, model) in [
            ("claude", "claude-opus-4-8"),
            ("claude", "claude-opus-5-5"),
            ("claude", "claude-sonnet-4-6"),
            ("codex", "gpt-5.5-codex"),
            ("codex", "gpt-99-mystery"),
        ] {
            let price = price_for(group, model, &empty()).expect("priced");
            assert!(price.long_context.is_none(), "{model} must stay untiered");
            for t in [&big, &small] {
                assert_eq!(
                    cost_usd(group, model, t, &empty()).to_bits(),
                    flat_formula(price, t).to_bits(),
                    "{model}"
                );
            }
            let mut all = TokenParts::from(&big);
            all.add(&TokenParts::from(&small));
            let summed = TokenCounts {
                input: all.input,
                output: all.output,
                cache_read: Some(all.cache_read),
                cache_creation: Some(all.cache_creation),
            };
            let agg = aggregate_cost(group, model, &all, &long_part(model, &big), &empty())
                .expect("priced");
            assert_eq!(
                agg.to_bits(),
                flat_formula(price, &summed).to_bits(),
                "{model}"
            );
        }
    }

    #[test]
    fn unknown_model_stays_unpriced_under_every_entry_point() {
        let t = tc(300_000, 1, Some(1), Some(1));
        approx(cost_usd("weirdgroup", "nope", &t, &empty()), 0.0);
        assert!(request_breakdown("weirdgroup", "nope", &t, &empty()).is_none());
        assert!(aggregate_cost(
            "weirdgroup",
            "nope",
            &TokenParts::from(&t),
            &long_part("nope", &t),
            &empty()
        )
        .is_none());
    }

    #[test]
    fn override_without_a_tier_drops_the_builtin_tier() {
        // An override REPLACES the whole row: no `long_context` means flat
        // rates for every request, even a long one on a tiered model.
        let mut overrides = HashMap::new();
        overrides.insert("grok-4.7".to_string(), ModelPrice::new(1.0, 1.0, 1.0, 1.0));
        let long = tc(300_000, 1_000_000, None, None);
        approx(cost_usd("grok", "grok-4.7", &long, &overrides), 1.3);
        let agg = aggregate_cost(
            "grok",
            "grok-4.7",
            &TokenParts::from(&long),
            &long_part("grok-4.7", &long),
            &overrides,
        )
        .expect("priced");
        approx(agg, 1.3);
    }

    #[test]
    fn override_with_a_tier_applies_at_the_builtin_threshold_even_on_an_untiered_model() {
        // Enabling a tier by config on an untiered model works, at the
        // model's built-in threshold (200k), per request and in aggregate.
        let mut overrides = HashMap::new();
        overrides.insert(
            "gpt-5.5-codex".to_string(),
            ModelPrice::new(5.0, 30.0, 0.5, 0.0).with_long_context(10.0, 45.0, 1.0, 0.0),
        );
        let short = tc(199_999, 1_000, None, None);
        let long = tc(200_000, 1_000, None, None);
        approx(
            cost_usd("codex", "gpt-5.5-codex", &short, &overrides),
            (199_999.0 * 5.0 + 1_000.0 * 30.0) / 1e6,
        );
        approx(
            cost_usd("codex", "gpt-5.5-codex", &long, &overrides),
            (200_000.0 * 10.0 + 1_000.0 * 45.0) / 1e6,
        );
        let mut all = TokenParts::from(&short);
        all.add(&TokenParts::from(&long));
        let mut lp = long_part("gpt-5.5-codex", &short);
        lp.add(&long_part("gpt-5.5-codex", &long));
        let agg = aggregate_cost("codex", "gpt-5.5-codex", &all, &lp, &overrides).expect("priced");
        approx(
            agg,
            cost_usd("codex", "gpt-5.5-codex", &short, &overrides)
                + cost_usd("codex", "gpt-5.5-codex", &long, &overrides),
        );
    }

    #[test]
    fn config_pricing_entries_stay_backward_compatible() {
        // A pre-tier config entry (no `long_context`) still loads, untiered,
        // and serializes back without the new key.
        let old: ModelPrice = serde_json::from_str(
            r#"{"input": 1.0, "output": 2.0, "cache_read": 0.1, "cache_creation": 1.25}"#,
        )
        .expect("old entry parses");
        assert_eq!(old, ModelPrice::new(1.0, 2.0, 0.1, 1.25));
        let json = serde_json::to_value(old).expect("serialize");
        assert!(json.get("long_context").is_none(), "no new key on the wire");
        // A tiered entry parses and round-trips.
        let tiered: ModelPrice = serde_json::from_str(
            r#"{"input": 2, "output": 6, "cache_read": 0.5, "cache_creation": 0,
                "long_context": {"input": 4, "output": 12, "cache_read": 1, "cache_creation": 0}}"#,
        )
        .expect("tiered entry parses");
        assert_eq!(tiered, GROK_4_7);
        let back: ModelPrice =
            serde_json::from_value(serde_json::to_value(tiered).expect("serialize"))
                .expect("round-trip");
        assert_eq!(back, tiered);
        // The threshold is not configurable: an attempt fails loudly.
        assert!(serde_json::from_str::<ModelPrice>(
            r#"{"input": 2, "output": 6, "cache_read": 0.5, "cache_creation": 0,
                "long_context": {"threshold": 100000, "input": 4, "output": 12,
                                 "cache_read": 1, "cache_creation": 0}}"#,
        )
        .is_err());
    }

    #[test]
    fn long_context_thresholds_cover_every_model_threshold() {
        let all = long_context_thresholds();
        assert!(all.contains(&DEFAULT_LONG_CONTEXT_THRESHOLD));
        assert!(all.windows(2).all(|w| w[0] < w[1]), "ascending, distinct");
        // Every resolvable tiered row's threshold is enumerated — the SQL
        // prompt-class query buckets on exactly this list.
        for slug in [
            "gpt-5.5",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6-luna",
            "grok-4.5",
            "grok-4.6",
            "grok-4.7",
        ] {
            let t = builtin_price(slug)
                .and_then(|p| p.long_context)
                .unwrap_or_else(|| panic!("{slug} is tiered"))
                .threshold;
            assert!(all.contains(&t), "{slug} threshold {t} enumerated");
        }
        assert_eq!(long_context_threshold("grok-4.7"), 200_000);
    }

    // ---- OpenAI (codex group) long-context tier: >272k, whole request ----

    #[test]
    fn openai_rows_tier_at_272k_on_the_whole_request() {
        // (model, short in/out/cached, long in/out/cached) per 1M — OpenAI
        // pricing page, read 2026-09-28.
        for (model, short, long) in [
            ("gpt-5.5", (5.0, 30.0, 0.5), (10.0, 45.0, 1.0)),
            ("gpt-5.6-sol", (4.0, 20.0, 0.4), (8.0, 30.0, 0.8)),
            ("gpt-5.6-terra", (2.0, 12.0, 0.2), (4.0, 18.0, 0.4)),
            ("gpt-5.6-luna", (0.2, 1.2, 0.02), (0.4, 1.8, 0.04)),
            ("gpt-6-astra", (10.0, 50.0, 1.0), (20.0, 75.0, 2.0)),
            ("gpt-6-sol", (2.0, 10.0, 0.2), (4.0, 15.0, 0.4)),
            ("gpt-6-luna", (0.1, 0.5, 0.01), (0.2, 0.75, 0.02)),
        ] {
            assert_eq!(long_context_threshold(model), 272_000, "{model}");
            // 271_999 prompt tokens is still short; 272_000 is long, and then
            // EVERY component (output and cached read included) reprices.
            let below = tc(1_999, 1_000, Some(270_000), None);
            let at = tc(2_000, 1_000, Some(270_000), None);
            approx(
                cost_usd("codex", model, &below, &empty()),
                (1_999.0 * short.0 + 1_000.0 * short.1 + 270_000.0 * short.2) / 1e6,
            );
            approx(
                cost_usd("codex", model, &at, &empty()),
                (2_000.0 * long.0 + 1_000.0 * long.1 + 270_000.0 * long.2) / 1e6,
            );
        }
    }

    #[test]
    fn openai_dated_snapshots_classify_like_they_price() {
        // The threshold follows the same exact-then-prefix resolution as the
        // price, so a dated snapshot is neither priced at the tier with the
        // wrong boundary nor classified with the grok default.
        for model in [
            "gpt-5.6-sol-20260709",
            "gpt-5.6-terra-20260709",
            "gpt-5.6-luna-20260709",
            "gpt-6-astra-20260903",
            "gpt-6-sol-20260922",
            "gpt-6-luna-20260922",
        ] {
            assert_eq!(long_context_threshold(model), 272_000, "{model}");
            let p = price_for("codex", model, &empty()).expect("priced");
            assert_eq!(
                p.long_context.expect("tiered").threshold,
                272_000,
                "{model}"
            );
        }
    }

    #[test]
    fn unverified_codex_models_stay_flat_and_classify_flat() {
        // `gpt-5.5-codex` is not on OpenAI's page and an unknown codex model
        // takes the group fallback: neither claims a tier, so neither can be
        // repriced (and the fold's classification agrees).
        for model in ["gpt-5.5-codex", "gpt-5.5-20260701", "gpt-99-mystery"] {
            let p = price_for("codex", model, &empty()).expect("priced");
            assert!(p.long_context.is_none(), "{model} is untiered");
            approx(
                cost_usd("codex", model, &tc(300_000, 1_000, None, None), &empty()),
                (300_000.0 * 5.0 + 1_000.0 * 30.0) / 1e6,
            );
        }
    }
}
