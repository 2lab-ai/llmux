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
//! Anthropic bills `cache_read` at 0.1× input and `cache_creation` at 1.25×
//! input; OpenAI/codex bills cached input at a flat discounted rate and has no
//! cache-creation charge. A per-model [`ModelPrice`] with four independent
//! rates (input / output / cache_read / cache_creation) expresses both
//! providers uniformly — codex models simply carry `cache_creation: 0.0`.
//!
//! All rates are **USD per 1,000,000 tokens**. Rates sourced: claude-api skill
//! cached 2026-06-04; OpenAI gpt-5.5 pricing 2026-04-23; Opus 5.5 from
//! anthropic.com/claude-opus-5-5, 2026-09-22.

use std::collections::HashMap;

use crate::tui::activity::normalize_model;
use crate::tui::TokenCounts;

/// Per-model price table entry. All four rates are **USD per 1,000,000
/// tokens**. A zero rate means "free / not charged" (e.g. codex has no
/// cache-creation charge → `cache_creation: 0.0`).
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
}

impl ModelPrice {
    const fn new(input: f64, output: f64, cache_read: f64, cache_creation: f64) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_creation,
        }
    }

    /// All-zero rate — the fallback for genuinely unknown (group, model) pairs.
    /// Yields `0.0` cost and never panics.
    pub const fn zero() -> Self {
        Self::new(0.0, 0.0, 0.0, 0.0)
    }
}

/// Opus-tier rates {input 5.0, output 25.0, cache_read 0.5, cache_creation 6.25}.
/// Also the `group == "claude"` unknown-model fallback.
const OPUS_TIER: ModelPrice = ModelPrice::new(5.0, 25.0, 0.5, 6.25);
/// Opus 5.5 (Anthropic announcement 2026-09-22): $4 in / $20 out / cache read
/// 0.20 / cache write 5.0 — 20% below the opus tier, cache reads 60% below, so
/// it must NOT fall to the `claude-opus-` prefix fallback.
const OPUS_5_5: ModelPrice = ModelPrice::new(4.0, 20.0, 0.20, 5.0);
/// Sonnet-tier rates {3.0, 15.0, 0.3, 3.75}.
const SONNET_TIER: ModelPrice = ModelPrice::new(3.0, 15.0, 0.3, 3.75);
/// Haiku-tier rates {1.0, 5.0, 0.1, 1.25}.
const HAIKU_TIER: ModelPrice = ModelPrice::new(1.0, 5.0, 0.1, 1.25);
/// Fable-family rates (Fable 5 and 5.1 share the tier) {10.0, 50.0, 1.0, 12.5}.
const FABLE_TIER: ModelPrice = ModelPrice::new(10.0, 50.0, 1.0, 12.5);
/// gpt-5.5 / codex default {input 5.0, output 30.0, cache_read 0.5,
/// cache_creation 0.0} (OpenAI pricing page, re-read 2026-09-28). Codex has no
/// cache-creation charge. Also the `group == "codex"` unknown-model fallback.
/// The page does not list `gpt-5.5-codex` / `gpt-5-codex`; they keep resolving
/// here by prefix / fallback, which is unverified.
const GPT_5_5: ModelPrice = ModelPrice::new(5.0, 30.0, 0.5, 0.0);
/// gpt-5.6-sol (flagship, 2026-07-09 launch, standard tier): $4 in / $20 out /
/// $0.40 cached input (OpenAI model page developers.openai.com/api/docs/models/
/// gpt-5.6-sol and the pricing page, read 2026-09-28). OpenAI calls this
/// promotional pricing "available at least through November 21, 2026", so
/// re-check it after that date. Codex: no cache-creation charge (the page's
/// $5 cache-write rate does not apply to subscription traffic). The >272k tier
/// ($8 in / $30 out) is not modeled.
const GPT_5_6_SOL: ModelPrice = ModelPrice::new(4.0, 20.0, 0.4, 0.0);
/// gpt-5.6-terra (mid tier): $2 in / $12 out / $0.20 cached input (OpenAI
/// pricing page, read 2026-09-28; same promotional caveat and conventions as
/// [`GPT_5_6_SOL`]).
const GPT_5_6_TERRA: ModelPrice = ModelPrice::new(2.0, 12.0, 0.2, 0.0);
/// gpt-5.6-luna (budget tier): $0.20 in / $1.20 out / $0.02 cached input
/// (OpenAI pricing page, read 2026-09-28; same caveats as [`GPT_5_6_SOL`]).
const GPT_5_6_LUNA: ModelPrice = ModelPrice::new(0.2, 1.2, 0.02, 0.0);
/// gpt-6-astra (generation-6 flagship, 2026-09 launch, standard tier): $10 in
/// / $50 out / $1 cached input (OpenAI API pricing page, read 2026-09-28).
/// Codex: no cache-creation charge, same convention as the other codex rows.
const GPT_6_ASTRA: ModelPrice = ModelPrice::new(10.0, 50.0, 1.0, 0.0);
/// gpt-6-sol (2026-09-22 launch, standard tier, prompts <=272k): $2 in / $10
/// out / $0.20 cached input (OpenAI API pricing page,
/// developers.openai.com/api/docs/pricing, read 2026-09-28). Codex: no
/// cache-creation charge, same convention as the other codex rows (the page's
/// separate $2.50 cache-write rate does not apply to subscription traffic).
/// The >272k-prompt tier ($4 in / $15 out) is not modeled.
const GPT_6_SOL: ModelPrice = ModelPrice::new(2.0, 10.0, 0.2, 0.0);
/// gpt-6.1-sol (2026-09-29 launch, standard tier, prompts <=272k): $2 in / $10
/// out / $0.10 cached input (OpenAI API pricing page,
/// developers.openai.com/api/docs/pricing, read 2026-09-29). Same conventions
/// as [`GPT_6_SOL`]: no cache-creation charge for codex traffic (the page's
/// $2.50 cache-write rate does not apply to subscription traffic), and the
/// tier for prompts over 272k ($4 in / $15 out / $0.20 cached) is not modeled
/// here. Cached input is half of gpt-6-sol's $0.20.
const GPT_6_1_SOL: ModelPrice = ModelPrice::new(2.0, 10.0, 0.1, 0.0);
/// gpt-6-luna (2026-09-22 launch, standard tier): $0.10 in / $0.50 out /
/// $0.01 cached input; the >272k tier ($0.20 in / $0.75 out) is not modeled.
/// Same sourcing and conventions as [`GPT_6_SOL`].
const GPT_6_LUNA: ModelPrice = ModelPrice::new(0.1, 0.5, 0.01, 0.0);
/// grok-4.5 (docs.x.ai, 2026-07-14): $2 in / $6 out, cached input 0.5, no
/// cache-creation charge. Also the `group == "grok"` unknown-model fallback.
/// Like the codex rows, an API-list-price EQUIVALENT for subscription
/// traffic, not a billed amount (docs/grok/spec.md §Compatibility).
const GROK_4_5: ModelPrice = ModelPrice::new(2.0, 6.0, 0.5, 0.0);
/// grok-4.6 (docs.x.ai, 2026-08-13): $2 in / $6 out; the $0.50/M cached input
/// was carried from grok-4.5 when this row landed and is now LISTED on the
/// page itself (docs.x.ai, re-read 2026-09-23) — same number, no longer an
/// inference. API-list-price equivalent for subscription traffic, like the
/// other grok rows.
const GROK_4_6: ModelPrice = ModelPrice::new(2.0, 6.0, 0.5, 0.0);
/// grok-4.7 (docs.x.ai pricing, 2026-09-23): $2 in / $6 out / $0.50 cached
/// input, no cache-creation charge — unchanged from grok-4.6. The
/// ≥200k-prompt long-context tier (rates double) is not modeled.
/// API-list-price equivalent for subscription traffic.
const GROK_4_7: ModelPrice = ModelPrice::new(2.0, 6.0, 0.5, 0.0);
/// Free — all four rates zero. Applied to the CURATED OpenRouter set, every
/// member of which had `pricing.prompt == "0"` and `pricing.completion == "0"`
/// on the live `GET /api/v1/models` probe of 2026-08-21
/// (docs/openrouter/spec.md). Deliberately NOT the `group == "openrouter"`
/// fallback: OpenRouter also serves ~400 PAID models reachable through the
/// `or-<vendor>/<slug>` escape hatch, and asserting $0 for those would be an
/// invented number.
const OPENROUTER_FREE: ModelPrice = ModelPrice::new(0.0, 0.0, 0.0, 0.0);

/// Look up the built-in default price for a *normalized*, lowercased model
/// slug. Exact matches first, then a sensible prefix fallback (so e.g.
/// `claude-opus-4-8-20260101` still resolves to the opus tier). Returns `None`
/// when nothing matches — callers apply the group fallback.
fn builtin_price(model_norm_lower: &str) -> Option<ModelPrice> {
    // Exact (post-normalization) matches.
    let exact = match model_norm_lower {
        "claude-opus-5-5" => Some(OPUS_5_5),
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6"
        | "claude-opus-4-5" => Some(OPUS_TIER),
        "claude-sonnet-4-6" | "claude-sonnet-4-5" => Some(SONNET_TIER),
        "claude-haiku-4-5" => Some(HAIKU_TIER),
        "claude-fable-5" | "claude-fable-5-1" => Some(FABLE_TIER),
        "gpt-5.5" => Some(GPT_5_5),
        "gpt-5.6" | "gpt-5.6-sol" => Some(GPT_5_6_SOL),
        "gpt-5.6-terra" => Some(GPT_5_6_TERRA),
        "gpt-5.6-luna" => Some(GPT_5_6_LUNA),
        "gpt-6" | "gpt-6-astra" => Some(GPT_6_ASTRA),
        "gpt-6-sol" => Some(GPT_6_SOL),
        "gpt-6.1-sol" => Some(GPT_6_1_SOL),
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
    if model_norm_lower.starts_with("claude-opus-5-5-") {
        // Opus 5.5 is CHEAPER than the opus tier ($4/$20 vs $5/$25), so a dated
        // snapshot must be caught here before the generic `claude-opus-` branch
        // below overcharges it — same ordering as `gpt-6-astra-` ahead of
        // `gpt-6-`. The trailing `-` is the version boundary: `claude-opus-5-50-*`
        // and `claude-opus-5-5x` are DIFFERENT models and must miss this branch
        // and fall to the opus tier.
        Some(OPUS_5_5)
    } else if model_norm_lower.starts_with("claude-opus-") {
        Some(OPUS_TIER)
    } else if model_norm_lower.starts_with("claude-sonnet-") {
        Some(SONNET_TIER)
    } else if model_norm_lower.starts_with("claude-haiku-") {
        Some(HAIKU_TIER)
    } else if model_norm_lower.starts_with("claude-fable-") {
        Some(FABLE_TIER)
    } else if model_norm_lower.starts_with("gpt-5.5-") {
        // Generation boundary: bare `gpt-5.5` matched exactly above; the
        // prefix branch requires the `-` so `gpt-5.50-*` never takes 5.5
        // rates (mirrors codex.rs `supports_extended_efforts`).
        Some(GPT_5_5)
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
    } else if model_norm_lower.starts_with("gpt-6.1-sol-") {
        Some(GPT_6_1_SOL)
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
        "codex" => Some(GPT_5_5),
        "grok" => Some(GROK_4_5),
        _ => None,
    }
}

/// API-equivalent USD cost for one [`TokenCounts`] under `(group, model)`'s
/// price. Unknown / zero-rate model → `0.0`. Never panics.
///
/// `cost = input·in/1e6 + output·out/1e6 + cache_read·cr/1e6 +
/// cache_creation·cc/1e6`; absent (`None`) cache fields contribute `0`.
pub fn cost_usd(
    group: &str,
    model: &str,
    tokens: &TokenCounts,
    overrides: &HashMap<String, ModelPrice>,
) -> f64 {
    cost_from_parts(
        group,
        model,
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_creation,
        overrides,
    )
}

/// Same as [`cost_usd`] but from the accumulated row fields the dashboard
/// already holds (`tokens_in`/`tokens_out` plus the `Option` cache counters on
/// [`crate::tui::activity::ModelUsage`]). `None` cache fields contribute `0`.
/// Unknown / zero-rate model → `0.0`. Never panics.
#[allow(clippy::too_many_arguments)]
pub fn cost_from_parts(
    group: &str,
    model: &str,
    tokens_in: u64,
    tokens_out: u64,
    cache_read: Option<u64>,
    cache_creation: Option<u64>,
    overrides: &HashMap<String, ModelPrice>,
) -> f64 {
    priced_cost(
        group,
        model,
        tokens_in,
        tokens_out,
        cache_read,
        cache_creation,
        overrides,
    )
    .unwrap_or(0.0)
}

/// [`cost_from_parts`] without the `0.0` sentinel: `None` means "no rate
/// known for this `(group, model)`", so a caller can never mistake a missing
/// rate for a free request (usage-stats review — the `priced` flag and the
/// cost must come from ONE lookup, not two calls that merely agree today).
#[allow(clippy::too_many_arguments)]
pub fn priced_cost(
    group: &str,
    model: &str,
    tokens_in: u64,
    tokens_out: u64,
    cache_read: Option<u64>,
    cache_creation: Option<u64>,
    overrides: &HashMap<String, ModelPrice>,
) -> Option<f64> {
    let price = price_for(group, model, overrides)?;
    let per_m = |count: u64, rate: f64| (count as f64) * rate / 1_000_000.0;
    Some(
        per_m(tokens_in, price.input)
            + per_m(tokens_out, price.output)
            + per_m(cache_read.unwrap_or(0), price.cache_read)
            + per_m(cache_creation.unwrap_or(0), price.cache_creation),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let p = price_for("grok", "grok-4.5", &overrides).expect("grok-4.5 priced");
        assert_eq!(
            (p.input, p.output, p.cache_read, p.cache_creation),
            (2.0, 6.0, 0.5, 0.0)
        );
        let p6 = price_for("grok", "grok-4.6", &overrides).expect("grok-4.6 priced");
        assert_eq!(
            (p6.input, p6.output, p6.cache_read, p6.cache_creation),
            (2.0, 6.0, 0.5, 0.0)
        );
        // grok-4.7 (docs.x.ai 2026-09-23): same rates as 4.6 — the
        // ≥200k-prompt long-context tier is deliberately not modeled.
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
        let cost = cost_usd("codex", "gpt-5.5", &tc(0, 1_000_000, None, None), &empty());
        approx(cost, 30.00);
    }

    #[test]
    fn gpt_5_5_has_no_cache_creation_charge() {
        // Codex never bills cache creation; even a huge count costs nothing for it.
        let cost = cost_usd(
            "codex",
            "gpt-5.5",
            &tc(0, 0, None, Some(1_000_000)),
            &empty(),
        );
        approx(cost, 0.0);
    }

    #[test]
    fn gpt_5_6_sol_matches_exact_and_bare_and_prefix() {
        // Exact `gpt-5.6-sol`, the bare `gpt-5.6` alias, and a future dated
        // snapshot all resolve to sol rates ($4 in / $20 out / $0.40 cache read).
        for model in ["gpt-5.6-sol", "gpt-5.6", "gpt-5.6-sol-20260709"] {
            let cost = cost_usd("codex", model, &tc(1_000_000, 0, None, None), &empty());
            approx(cost, 4.00);
            let out = cost_usd("codex", model, &tc(0, 1_000_000, None, None), &empty());
            approx(out, 20.00);
            let cached = cost_usd("codex", model, &tc(0, 0, Some(1_000_000), None), &empty());
            approx(cached, 0.40);
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
        // Codex convention: no cache-creation charge.
        let creation = cost_usd(
            "codex",
            "gpt-6-astra",
            &tc(0, 0, None, Some(1_000_000)),
            &empty(),
        );
        approx(creation, 0.0);
        // The bare ALIAS is deliberately absent from the price table: the
        // codex provider resolves `astra` to `gpt-6-astra` BEFORE the request
        // (and the recorded model) leaves llmux, so pricing only ever sees the
        // resolved slug. Pricing the alias too would be a second source of
        // truth that could silently disagree with the wire model.
        assert_eq!(builtin_price("astra"), None);
        assert_eq!(
            price_for("codex", "astra", &empty()),
            Some(GPT_5_5),
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
            // gpt-6.1-sol: same $2 / $10 as gpt-6-sol but $0.10 cached input.
            ("gpt-6.1-sol", 2.00, 10.00, 0.10),
            ("gpt-6.1-sol-20260929", 2.00, 10.00, 0.10),
            ("gpt-6-luna", 0.10, 0.50, 0.01),
            ("gpt-6-luna-20260922", 0.10, 0.50, 0.01),
        ] {
            approx(
                cost_usd("codex", model, &tc(1_000_000, 0, None, None), &empty()),
                input,
            );
            approx(
                cost_usd("codex", model, &tc(0, 1_000_000, None, None), &empty()),
                output,
            );
            approx(
                cost_usd("codex", model, &tc(0, 0, Some(1_000_000), None), &empty()),
                cached,
            );
            // Codex convention: no cache-creation charge.
            approx(
                cost_usd("codex", model, &tc(0, 0, None, Some(1_000_000)), &empty()),
                0.0,
            );
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
            Some(GPT_5_5),
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
            approx(
                cost_usd("codex", model, &tc(1_000_000, 0, None, None), &empty()),
                input,
            );
            approx(
                cost_usd("codex", model, &tc(0, 1_000_000, None, None), &empty()),
                output,
            );
            approx(
                cost_usd("codex", model, &tc(0, 0, Some(1_000_000), None), &empty()),
                cached,
            );
        }
    }

    #[test]
    fn gpt_5_6_has_no_cache_creation_charge() {
        let cost = cost_usd(
            "codex",
            "gpt-5.6-sol",
            &tc(0, 0, None, Some(1_000_000)),
            &empty(),
        );
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
            Some(GPT_5_5),
            "unknown codex model takes the group fallback"
        );
        // The boundary must not break real dated snapshots of the family.
        assert_eq!(builtin_price("gpt-5.6-sol-20260709"), Some(GPT_5_6_SOL));
        assert_eq!(builtin_price("gpt-5.6-terra-20260709"), Some(GPT_5_6_TERRA));
        assert_eq!(builtin_price("gpt-5.6-luna-20260709"), Some(GPT_5_6_LUNA));
        assert_eq!(builtin_price("gpt-5.5-codex"), Some(GPT_5_5));
    }

    #[test]
    fn gpt_5_6_sol_cache_read_is_ten_percent_of_input() {
        // OpenAI bills cached input at the flat gpt-5.x discount (10% of the
        // input rate). gpt-5.6-sol input is $4/1e6, so 1e6 cache-read tokens
        // cost $0.40 — a third of a mostly-cached prompt is billed at a tenth,
        // not the full input rate (the codex cache-read cost regression).
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
    fn cost_from_parts_matches_cost_usd() {
        let tokens = tc(700, 300, Some(120), None);
        let a = cost_usd("codex", "gpt-5.5", &tokens, &empty());
        let b = cost_from_parts("codex", "gpt-5.5", 700, 300, Some(120), None, &empty());
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
}
