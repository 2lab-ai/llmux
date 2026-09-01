//! Model-aware backend-group routing (PURE).
//!
//! An inbound Anthropic Messages request names a `model`; this module maps
//! that name to a [`BackendGroup`] — the pool of accounts that can serve it.
//! Four groups exist: [`BackendGroup::Claude`] (oauth + apikey accounts,
//! served by the Anthropic provider), [`BackendGroup::Codex`] (chatgpt
//! oauth accounts, served by the codex provider), [`BackendGroup::Grok`]
//! (xAI grok oauth accounts, served by the grok provider), and
//! [`BackendGroup::OpenRouter`] (OpenRouter API-key accounts, served by the
//! openrouter provider — a passthrough, NOT a translator, because OpenRouter
//! exposes a native Anthropic Messages endpoint; docs/openrouter/spec.md).
//! The scheduler then picks the best eligible account *within* that group,
//! sticky per group.
//!
//! Everything here is a deterministic function of its inputs — no IO, no
//! clock, no shared state — so it is unit-test heavy by design. The
//! classifier is built once from config (or the builtin defaults) and shared
//! read-only behind an `Arc`.

/// Which backend pool an account belongs to / a model routes to.
///
/// `Ord` is derived so the group can key a `BTreeMap` (per-group stickiness)
/// with a stable, total order: `Claude < Codex < Grok < OpenRouter`. That
/// order also makes `Claude` the representative group when a scalar must be
/// chosen (status output picks the claude slot first) and fixes the
/// `on_empty_group` fallback scan order (docs/grok/spec.md §R5). OpenRouter is
/// appended LAST precisely so neither of those established behaviors moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BackendGroup {
    Claude,
    Codex,
    Grok,
    OpenRouter,
}

impl BackendGroup {
    /// Group an account belongs to, derived from its credential `kind`
    /// (`"oauth" | "apikey" | "codex" | "grok" | "openrouter"` — see
    /// [`crate::config::AccountCredential::kind`]). Codex credentials are the
    /// Codex group, grok credentials the Grok group, openrouter credentials
    /// the OpenRouter group; everything else is Claude.
    pub fn from_kind(kind: &str) -> Self {
        match kind {
            "codex" => Self::Codex,
            "grok" => Self::Grok,
            "openrouter" => Self::OpenRouter,
            _ => Self::Claude,
        }
    }

    /// Every group, in the canonical `Ord` order
    /// (`Claude < Codex < Grok < OpenRouter`). Single source for "scan all
    /// groups" loops (fallback resolution, status rendering).
    pub const ALL: &'static [BackendGroup] =
        &[Self::Claude, Self::Codex, Self::Grok, Self::OpenRouter];

    /// Lowercase label for logs / status output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::OpenRouter => "openrouter",
        }
    }

    /// Inverse of [`Self::as_str`]: parse a group label back into the enum.
    /// Unknown labels degrade to [`Self::Claude`] (the representative group),
    /// so a newer server's group never strands the display.
    pub fn from_label(label: &str) -> Self {
        match label {
            "codex" => Self::Codex,
            "grok" => Self::Grok,
            "openrouter" => Self::OpenRouter,
            _ => Self::Claude,
        }
    }

    /// Whether this group's provider must TRANSLATE the Anthropic Messages
    /// body into a foreign wire format (and its stream back) — codex and grok
    /// (Messages↔Responses + an SSE converter). Claude is the native upstream;
    /// OpenRouter serves the Anthropic Messages format natively too
    /// (docs/openrouter/spec.md), so both are byte-level passthroughs.
    ///
    /// Exhaustive on purpose: a fifth group is a COMPILE ERROR here rather
    /// than a silent default. Getting this wrong is body corruption, not
    /// degradation — a native-Messages backend wrongly marked as translating
    /// would have its payload run through the Responses converter.
    pub fn needs_body_translation(self) -> bool {
        match self {
            Self::Codex | Self::Grok => true,
            Self::Claude | Self::OpenRouter => false,
        }
    }

    /// Whether this group's upstream implements ONLY `POST /v1/messages` —
    /// i.e. it has no `/v1/messages/count_tokens` sibling and no other
    /// Anthropic endpoint. True for every non-Anthropic group.
    ///
    /// This is a DIFFERENT question from [`Self::needs_body_translation`], and
    /// conflating them has already cost one defect: `count_tokens`'s local
    /// estimate used to hang off the translate flag, so when OpenRouter joined
    /// as a non-translating group its `count_tokens` calls were proxied to an
    /// endpoint that live-probes 404 (2026-08-21) — on a path Claude Code hits
    /// for every context measurement. Two questions, two predicates, both
    /// exhaustive.
    pub fn serves_messages_only(self) -> bool {
        match self {
            Self::Codex | Self::Grok | Self::OpenRouter => true,
            Self::Claude => false,
        }
    }
}

impl std::fmt::Display for BackendGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One model-matching rule. A model string (already lowercased) matches when
/// it satisfies the `kind`; the first matching rule (config order, then
/// builtins) decides the group.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Rule {
    /// Matches when the model starts with this (lowercase) prefix.
    Prefix(String),
    /// Matches when the model contains this (lowercase) substring.
    Substring(String),
    /// Matches the model exactly (lowercase).
    Exact(String),
}

impl Rule {
    fn matches(&self, model_lower: &str) -> bool {
        match self {
            Rule::Prefix(p) => model_lower.starts_with(p.as_str()),
            Rule::Substring(s) => model_lower.contains(s.as_str()),
            Rule::Exact(e) => model_lower == e.as_str(),
        }
    }

    /// Parse a config rule token. `"codex"`-style bare words are substrings;
    /// `*`/wildcards are not supported — a token is treated as a PREFIX rule
    /// unless it is wrapped: `~substr` → substring, `=exact` → exact. This
    /// keeps the common config case (`"claude-"`, `"gpt-"`) a simple prefix
    /// while still allowing the two builtin special cases to be expressed.
    fn parse(token: &str) -> Self {
        let token = token.trim().to_ascii_lowercase();
        if let Some(rest) = token.strip_prefix('~') {
            Rule::Substring(rest.to_string())
        } else if let Some(rest) = token.strip_prefix('=') {
            Rule::Exact(rest.to_string())
        } else {
            Rule::Prefix(token)
        }
    }
}

/// Builtin codex rules: `gpt-` / `o1`-`o4` prefixes, `codex` substring, the
/// exact `gpt-5.5` / `gpt-5.6` family ids (covered by `gpt-` already, but kept
/// explicit so the intent survives a future prefix change), and the bare
/// variant aliases `sol` / `terra` / `luna` (which the codex provider resolves
/// to the latest gpt generation of that variant — see
/// [`crate::provider::codex`]).
fn builtin_codex_rules() -> Vec<Rule> {
    vec![
        Rule::Prefix("gpt-".to_string()),
        Rule::Prefix("o1".to_string()),
        Rule::Prefix("o3".to_string()),
        Rule::Prefix("o4".to_string()),
        Rule::Substring("codex".to_string()),
        Rule::Exact("gpt-5.5".to_string()),
        Rule::Exact("gpt-5.6".to_string()),
        Rule::Exact("gpt-5.6-sol".to_string()),
        Rule::Exact("sol".to_string()),
        Rule::Exact("terra".to_string()),
        Rule::Exact("luna".to_string()),
    ]
}

/// Builtin claude rules: the Anthropic model families plus the fable alias.
fn builtin_claude_rules() -> Vec<Rule> {
    vec![
        Rule::Prefix("claude".to_string()),
        Rule::Prefix("opus".to_string()),
        Rule::Prefix("sonnet".to_string()),
        Rule::Prefix("haiku".to_string()),
        Rule::Prefix("fable".to_string()),
    ]
}

/// Builtin grok rules: the xAI model family (`grok-4.5`, `grok-build-0.1`, …).
fn builtin_grok_rules() -> Vec<Rule> {
    vec![Rule::Prefix("grok".to_string())]
}

/// Builtin openrouter rules (docs/openrouter/spec.md §R2): the user-facing
/// `or-` model prefix (`or-ox-alpha`, `or-glm-5.2`, `or-openai/gpt-oss-20b:free`),
/// the bare `or` family alias (resolves to the live free-model pin, mirroring
/// bare `grok`), and the raw `openrouter/` vendor prefix so OpenRouter's own
/// router slugs (`openrouter/free`) route here when named verbatim.
///
/// Disjointness from the other builtin families is checked in the unit tests
/// below: codex owns `gpt-`/`o1`/`o3`/`o4`/`~codex`, claude owns
/// `claude|opus|sonnet|haiku|fable`, grok owns `grok` — none of which is a
/// prefix of `or-` or of the bare `or`.
fn builtin_openrouter_rules() -> Vec<Rule> {
    vec![
        Rule::Prefix("or-".to_string()),
        Rule::Exact("or".to_string()),
        Rule::Prefix("openrouter/".to_string()),
    ]
}

/// Compiled model→group classifier. First-match-wins over the codex rules
/// then the claude rules; an unmatched (or absent) model falls back to
/// `default_group`. Built from config overrides when present, else builtins.
#[derive(Debug, Clone)]
pub struct Classifier {
    codex_rules: Vec<Rule>,
    claude_rules: Vec<Rule>,
    grok_rules: Vec<Rule>,
    openrouter_rules: Vec<Rule>,
    default_group: BackendGroup,
}

impl Default for Classifier {
    /// Builtin defaults: codex = `gpt-`/`o1`/`o3`/`o4` prefixes + `codex`
    /// substring + exact `gpt-5.5`; claude = the Anthropic families; fallback
    /// = Claude.
    fn default() -> Self {
        Self {
            codex_rules: builtin_codex_rules(),
            claude_rules: builtin_claude_rules(),
            grok_rules: builtin_grok_rules(),
            openrouter_rules: builtin_openrouter_rules(),
            default_group: BackendGroup::Claude,
        }
    }
}

impl Classifier {
    /// Build from config-supplied model lists. An EMPTY list for a group
    /// keeps that group's builtin rules (so partial config doesn't silently
    /// drop a whole family); a non-empty list REPLACES the builtins for that
    /// group (config override beats builtin). `default_group` is parsed from
    /// the config string (`"codex"` → Codex, anything else → Claude).
    pub fn from_config(
        claude_models: &[String],
        codex_models: &[String],
        grok_models: &[String],
        openrouter_models: &[String],
        default_group: &str,
    ) -> Self {
        let claude_rules = if claude_models.is_empty() {
            builtin_claude_rules()
        } else {
            claude_models.iter().map(|m| Rule::parse(m)).collect()
        };
        let codex_rules = if codex_models.is_empty() {
            builtin_codex_rules()
        } else {
            codex_models.iter().map(|m| Rule::parse(m)).collect()
        };
        let grok_rules = if grok_models.is_empty() {
            builtin_grok_rules()
        } else {
            grok_models.iter().map(|m| Rule::parse(m)).collect()
        };
        let openrouter_rules = if openrouter_models.is_empty() {
            builtin_openrouter_rules()
        } else {
            openrouter_models.iter().map(|m| Rule::parse(m)).collect()
        };
        Self {
            codex_rules,
            claude_rules,
            grok_rules,
            openrouter_rules,
            // One label→group parser for the whole crate
            // ([`BackendGroup::from_label`]): a second hand-rolled map here
            // means every new group must be added twice, and missing THIS one
            // degrades a valid `routing.default_group` to Claude silently
            // while status/API parsing still recognizes it.
            default_group: BackendGroup::from_label(
                default_group.trim().to_ascii_lowercase().as_str(),
            ),
        }
    }

    /// Classify a model name to a group. The name is TRIMMED and
    /// ASCII-lowercased before rule matching, so the rules see exactly the
    /// string the alias resolvers see — `catalog::resolve_claude_alias` and
    /// the Anthropic provider's `resolve_client_alias` both trim+lowercase,
    /// and the alias contract is advertised as whitespace-tolerant. Without
    /// the trim, `"  opus"` defeats `Rule::Prefix("opus")` (`starts_with`) and
    /// only lands on claude by luck of `default_group` — under a non-claude
    /// default it would route to the wrong backend. Leading/trailing
    /// whitespace is never meaningful in a model slug for any group.
    ///
    /// One trailing `[1m]` context-window suffix is also stripped: it is
    /// client-side display metadata (Claude Code derives its context readout
    /// from the model string), so `sol[1m]` must classify exactly like `sol`.
    /// Prefix rules such as `gpt-` survive the suffix on their own, but EXACT
    /// rules — the bare variant aliases — do not, and would land on
    /// `default_group` (the wrong backend).
    ///
    /// `None` (no model in the body) routes to the configured default group.
    /// Codex rules are checked first, then claude, then grok; an unrecognized
    /// model falls back to `default_group`. (The rule families are disjoint by
    /// construction — `gpt-`/`o*` vs Anthropic names vs `grok` — so the check
    /// order only decides ties a user creates with overlapping config
    /// overrides.)
    pub fn classify(&self, model: Option<&str>) -> BackendGroup {
        let Some(model) = model else {
            return self.default_group;
        };
        let lower = model.trim().to_ascii_lowercase();
        let lower = lower
            .strip_suffix("[1m]")
            .map(str::to_string)
            .unwrap_or(lower);
        // OpenRouter FIRST. Its builtin selectors are explicit and anchored
        // (`or-` / `openrouter/` prefixes, exact `or`), while codex owns the
        // greedy `Substring("codex")` — so a perfectly valid escape-hatch slug
        // like `or-openai/gpt-5-codex` would otherwise be stolen by codex and
        // sent to the ChatGPT backend, which has never heard of it. Most
        // specific selector wins; this is the only pair where the builtin
        // families are not disjoint.
        if self.openrouter_rules.iter().any(|r| r.matches(&lower)) {
            return BackendGroup::OpenRouter;
        }
        if self.codex_rules.iter().any(|r| r.matches(&lower)) {
            return BackendGroup::Codex;
        }
        if self.claude_rules.iter().any(|r| r.matches(&lower)) {
            return BackendGroup::Claude;
        }
        if self.grok_rules.iter().any(|r| r.matches(&lower)) {
            return BackendGroup::Grok;
        }
        self.default_group
    }
}

/// Extract the `model` field from an Anthropic Messages JSON body, if any.
/// A non-JSON body, or one with no string `model`, yields `None` — the
/// classifier then routes by the default group. This is the single source of
/// the model-extraction logic the Anthropic provider's `request_out` reuses.
pub fn model_from_body(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()?
        .get("model")?
        .as_str()
        .map(str::to_string)
}

/// The scope label the Anthropic usage poll uses for the per-model weekly
/// bucket that governs Fable requests (`limits[].scope.model.display_name`,
/// verbatim "Fable" — `.prd/13-usage-raw-sources.md` §Carrier 1). Single source
/// of the label that keys Fable-scoped cooldowns and the `fable_weekly`
/// accessor; matched case-insensitively everywhere it is used.
pub const FABLE_SCOPE_LABEL: &str = "Fable";

/// Model families that route to the Fable weekly bucket. CENTRAL registry —
/// the ONLY place a model string is decided to be Fable-scoped, so the
/// scheduler never grows ad-hoc `contains("fable")` checks. Extend here when a
/// new Fable-family id appears.
const FABLE_FAMILIES: &[&str] = &["fable"];

/// Whether a requested model is Fable-family (case-insensitive). `None` (no
/// model in the body) is NOT Fable — such a request routes by the default group
/// and must never be treated as Fable-scoped. The scheduler uses this to decide
/// whether a request is additionally gated by an account's Fable-scoped
/// cooldown / preemptive Fable-critical exclusion; non-Fable requests ignore
/// that state entirely.
///
/// Uses `contains` (not `starts_with`) deliberately: the only observed id is
/// `fable-5` but a vendor-prefixed variant (`claude-fable-…`) must still be
/// caught, and "fable" is distinctive enough that a false positive is
/// negligible. Being conservative here would wrongly park a Fable-exhausted
/// account's whole capacity — the exact bug W2 fixes — so the classifier errs
/// toward recognizing Fable.
pub fn is_fable_model(model: Option<&str>) -> bool {
    let Some(model) = model else {
        return false;
    };
    let lower = model.to_ascii_lowercase();
    FABLE_FAMILIES.iter().any(|fam| lower.contains(fam))
}

/// Extract `metadata.user_id` from an Anthropic Messages JSON body, if any.
/// This is the keyless per-client attribution identity for proxy metering
/// (issue #32): present in ~98.9% of real requests and stable per session,
/// account-independent. A non-JSON body, a missing `metadata`/`user_id`, or a
/// non-string `user_id` yields `None` — the metering layer then attributes the
/// request to the explicit `unknown` bucket rather than dropping it. This is
/// purely for *counting*: it never gates the request or issues a key.
pub fn user_id_from_body(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()?
        .get("metadata")?
        .get("user_id")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> Classifier {
        Classifier::default()
    }

    // ---- from_kind ----

    #[test]
    fn from_kind_maps_codex_credential_to_codex_group() {
        assert_eq!(BackendGroup::from_kind("codex"), BackendGroup::Codex);
    }

    #[test]
    fn from_kind_maps_oauth_and_apikey_to_claude_group() {
        assert_eq!(BackendGroup::from_kind("oauth"), BackendGroup::Claude);
        assert_eq!(BackendGroup::from_kind("apikey"), BackendGroup::Claude);
        assert_eq!(
            BackendGroup::from_kind("anything-else"),
            BackendGroup::Claude
        );
    }

    // ---- from_label (inverse of as_str) ----

    #[test]
    fn from_label_is_inverse_of_as_str() {
        for g in [BackendGroup::Claude, BackendGroup::Codex] {
            assert_eq!(BackendGroup::from_label(g.as_str()), g);
        }
    }

    #[test]
    fn from_label_defaults_unknown_to_claude() {
        assert_eq!(BackendGroup::from_label("mystery"), BackendGroup::Claude);
    }

    // ---- builtin codex rules ----

    #[test]
    fn gpt_prefix_routes_to_codex() {
        assert_eq!(builtin().classify(Some("gpt-4o")), BackendGroup::Codex);
        assert_eq!(builtin().classify(Some("gpt-5")), BackendGroup::Codex);
    }

    #[test]
    fn exact_gpt_5_5_routes_to_codex() {
        assert_eq!(builtin().classify(Some("gpt-5.5")), BackendGroup::Codex);
    }

    #[test]
    fn gpt_5_5_with_context_suffix_still_routes_to_codex() {
        // Claude Code derives the displayed context window from the model-name
        // string client-side; the `[1m]` suffix opts into a larger window and
        // is stripped before the request leaves the client. llmux must
        // still route `gpt-5.5[1m]` to codex (the `gpt-` prefix matches), so a
        // user can get a larger window readout while staying on codex. (req9-B)
        assert_eq!(builtin().classify(Some("gpt-5.5[1m]")), BackendGroup::Codex);
        assert_eq!(
            builtin().classify(Some("gpt-5.5-codex")),
            BackendGroup::Codex
        );
    }

    #[test]
    fn bare_alias_with_context_suffix_still_routes_to_codex() {
        // The bare aliases are EXACT rules, so without the suffix strip
        // `sol[1m]` would match nothing and fall to the default group.
        assert_eq!(builtin().classify(Some("sol[1m]")), BackendGroup::Codex);
        assert_eq!(builtin().classify(Some("terra[1m]")), BackendGroup::Codex);
        assert_eq!(builtin().classify(Some("SOL[1M]")), BackendGroup::Codex);
        // Prefix-matched ids are unaffected.
        assert_eq!(
            builtin().classify(Some("gpt-5.6-sol[1m]")),
            BackendGroup::Codex
        );
    }

    #[test]
    fn context_suffix_strip_is_global_across_groups() {
        // The strip is deliberately group-agnostic: `[1m]` is client display
        // metadata that can ride ANY backend's model string, so classification
        // must be identical with and without it.
        assert_eq!(
            builtin().classify(Some("claude-sonnet-5[1m]")),
            BackendGroup::Claude
        );
        assert_eq!(builtin().classify(Some("sonnet[1m]")), BackendGroup::Claude);
        assert_eq!(builtin().classify(Some("grok-4.6[1m]")), BackendGroup::Grok);
        assert_eq!(builtin().classify(Some("grok[1m]")), BackendGroup::Grok);
    }

    #[test]
    fn o_series_prefixes_route_to_codex() {
        assert_eq!(builtin().classify(Some("o1")), BackendGroup::Codex);
        assert_eq!(builtin().classify(Some("o1-mini")), BackendGroup::Codex);
        assert_eq!(builtin().classify(Some("o3")), BackendGroup::Codex);
        assert_eq!(builtin().classify(Some("o3-pro")), BackendGroup::Codex);
        assert_eq!(builtin().classify(Some("o4-mini")), BackendGroup::Codex);
    }

    #[test]
    fn bare_variant_aliases_route_to_codex() {
        // The bare variant aliases resolve upstream to the latest gpt
        // generation of that variant; routing classifies them to codex.
        assert_eq!(builtin().classify(Some("sol")), BackendGroup::Codex);
        assert_eq!(builtin().classify(Some("terra")), BackendGroup::Codex);
        assert_eq!(builtin().classify(Some("luna")), BackendGroup::Codex);
        // Exact, case-insensitive like the rest.
        assert_eq!(builtin().classify(Some("SOL")), BackendGroup::Codex);
        // Not a substring rule: a name merely containing "sol" is unaffected.
        assert_eq!(
            builtin().classify(Some("solar-flare")),
            BackendGroup::Claude
        );
    }

    #[test]
    fn codex_substring_routes_to_codex() {
        assert_eq!(builtin().classify(Some("codex")), BackendGroup::Codex);
        assert_eq!(
            builtin().classify(Some("some-codex-model")),
            BackendGroup::Codex
        );
    }

    // ---- builtin claude rules ----

    #[test]
    fn claude_families_route_to_claude() {
        assert_eq!(
            builtin().classify(Some("claude-sonnet-4-5")),
            BackendGroup::Claude
        );
        assert_eq!(builtin().classify(Some("opus")), BackendGroup::Claude);
        assert_eq!(builtin().classify(Some("opus-4-1")), BackendGroup::Claude);
        assert_eq!(builtin().classify(Some("sonnet")), BackendGroup::Claude);
        assert_eq!(builtin().classify(Some("haiku")), BackendGroup::Claude);
        assert_eq!(builtin().classify(Some("fable-5")), BackendGroup::Claude);
    }

    // ---- case-insensitivity ----

    #[test]
    fn classification_is_case_insensitive() {
        assert_eq!(builtin().classify(Some("GPT-5.5")), BackendGroup::Codex);
        assert_eq!(builtin().classify(Some("Gpt-4O")), BackendGroup::Codex);
        assert_eq!(builtin().classify(Some("OPUS")), BackendGroup::Claude);
        assert_eq!(
            builtin().classify(Some("Claude-Sonnet-4-5")),
            BackendGroup::Claude
        );
        assert_eq!(builtin().classify(Some("CODEX")), BackendGroup::Codex);
    }

    #[test]
    fn classify_is_case_insensitive() {
        let c = builtin();
        assert_eq!(c.classify(Some("OPUS")), BackendGroup::Claude);
        assert_eq!(c.classify(Some("Grok-4.5")), BackendGroup::Grok);
    }

    // ---- whitespace tolerance (agrees with catalog::resolve_claude_alias) ----

    #[test]
    fn classify_trims_whitespace_before_matching() {
        let c = builtin();
        assert_eq!(c.classify(Some("  opus  ")), BackendGroup::Claude);
        assert_eq!(c.classify(Some("\topus")), BackendGroup::Claude);
        assert_eq!(c.classify(Some("  gpt-5.6-sol ")), BackendGroup::Codex);
        assert_eq!(c.classify(Some("  grok-4.5")), BackendGroup::Grok);

        // The claude assertions above would also pass by luck of the default
        // group being claude. Re-run them with a non-claude default so the
        // padded alias must match the CLAUDE RULE, not the fallback.
        let grok_default = Classifier::from_config(&[], &[], &[], &[], "grok");
        assert_eq!(
            grok_default.classify(Some("  opus  ")),
            BackendGroup::Claude,
            "padded claude alias must route by rule, not by default_group"
        );
        assert_eq!(
            grok_default.classify(Some("\topus")),
            BackendGroup::Claude,
            "padded claude alias must route by rule, not by default_group"
        );
    }

    // ---- None / unknown → default fallback ----

    #[test]
    fn none_model_routes_to_default_group() {
        assert_eq!(builtin().classify(None), BackendGroup::Claude);
    }

    #[test]
    fn unknown_model_falls_back_to_claude() {
        assert_eq!(builtin().classify(Some("llama-3")), BackendGroup::Claude);
        assert_eq!(
            builtin().classify(Some("mistral-large")),
            BackendGroup::Claude
        );
        assert_eq!(builtin().classify(Some("")), BackendGroup::Claude);
    }

    // ---- first-match-wins (codex checked before claude) ----

    #[test]
    fn codex_rule_wins_when_both_could_match() {
        // A contrived name containing both a codex substring and a claude
        // prefix: codex rules are evaluated first, so it routes to codex.
        assert_eq!(
            builtin().classify(Some("claude-codex-hybrid")),
            BackendGroup::Codex,
            "codex substring is matched before the claude prefix"
        );
    }

    // ---- config override beats builtin ----

    // ---- C2: grok routing ----

    #[test]
    fn c2_grok_prefix_routes_grok_and_others_unchanged() {
        let c = builtin();
        assert_eq!(c.classify(Some("grok-4.5")), BackendGroup::Grok);
        assert_eq!(c.classify(Some("GROK-BUILD-0.1")), BackendGroup::Grok);
        assert_eq!(c.classify(Some("gpt-5.6-sol")), BackendGroup::Codex);
        assert_eq!(c.classify(Some("claude-sonnet-5")), BackendGroup::Claude);
        assert_eq!(c.classify(Some("mystery-model")), BackendGroup::Claude);
        assert_eq!(c.classify(None), BackendGroup::Claude);
    }

    #[test]
    fn c2_grok_kind_and_label_round_trip() {
        assert_eq!(BackendGroup::from_kind("grok"), BackendGroup::Grok);
        assert_eq!(BackendGroup::Grok.as_str(), "grok");
        assert_eq!(BackendGroup::from_label("grok"), BackendGroup::Grok);
        assert_eq!(
            BackendGroup::ALL,
            &[
                BackendGroup::Claude,
                BackendGroup::Codex,
                BackendGroup::Grok,
                BackendGroup::OpenRouter
            ]
        );
    }

    #[test]
    fn openrouter_kind_and_label_round_trip() {
        assert_eq!(
            BackendGroup::from_kind("openrouter"),
            BackendGroup::OpenRouter
        );
        assert_eq!(BackendGroup::OpenRouter.as_str(), "openrouter");
        assert_eq!(
            BackendGroup::from_label("openrouter"),
            BackendGroup::OpenRouter
        );
        // OpenRouter is ordered LAST so the representative group stays Claude
        // and the `on_empty_group` fallback scan order is unchanged.
        assert!(BackendGroup::Grok < BackendGroup::OpenRouter);
        assert_eq!(
            BackendGroup::ALL.iter().min(),
            Some(&BackendGroup::Claude),
            "claude must remain the representative group"
        );
    }

    #[test]
    fn openrouter_builtin_rules_classify_the_or_family() {
        let c = Classifier::default();
        for model in [
            "or-ox-alpha",
            "or-glm-5.2",
            "or-gpt-oss-20b",
            // The verbatim escape hatch: a full vendor slug behind `or-`.
            "or-openai/gpt-oss-20b:free",
            // OpenRouter's own router slug, named without the `or-` prefix.
            "openrouter/free",
        ] {
            assert_eq!(
                c.classify(Some(model)),
                BackendGroup::OpenRouter,
                "{model} must route to the openrouter group"
            );
        }
        // Bare family alias + the classifier's whitespace/case/[1m] handling.
        assert_eq!(c.classify(Some("or")), BackendGroup::OpenRouter);
        assert_eq!(c.classify(Some("  OR  ")), BackendGroup::OpenRouter);
        assert_eq!(
            c.classify(Some("or-ox-alpha[1m]")),
            BackendGroup::OpenRouter
        );
    }

    /// The escape hatch names REAL upstream slugs, and some of them contain
    /// another family's marker. `or-openai/gpt-5-codex` is a valid OpenRouter
    /// model whose slug contains "codex" — codex's builtin rule is a greedy
    /// `Substring("codex")`, so without openrouter being checked FIRST it is
    /// routed to the ChatGPT backend, which has never heard of it.
    #[test]
    fn explicit_or_prefix_beats_codex_greedy_substring() {
        let c = Classifier::default();
        for model in [
            "or-openai/gpt-5-codex",
            "or-codex-something",
            // …and the other families' markers, for the same reason.
            "or-anthropic/claude-sonnet-4",
            "or-x-ai/grok-3",
            "or-openai/gpt-oss-20b:free",
        ] {
            assert_eq!(
                c.classify(Some(model)),
                BackendGroup::OpenRouter,
                "{model} is an OpenRouter slug, not another family's"
            );
        }
        // The other families keep their own ids — the `or-` anchor is what
        // decides, not the presence of a vendor name.
        assert_eq!(c.classify(Some("gpt-5-codex")), BackendGroup::Codex);
        assert_eq!(c.classify(Some("codex-mini")), BackendGroup::Codex);
        assert_eq!(c.classify(Some("claude-sonnet-4")), BackendGroup::Claude);
        assert_eq!(c.classify(Some("grok-4.6")), BackendGroup::Grok);
    }

    #[test]
    fn openrouter_rules_are_disjoint_from_the_other_builtin_families() {
        // The claim made in `builtin_openrouter_rules`'s doc comment, made
        // mechanical: no existing family's model may be captured by the `or-`
        // rules, and no `or-` model may be captured by an existing family.
        // The `o1`/`o3`/`o4` codex prefixes are the near-miss worth pinning —
        // they are PREFIXES, not an `o*` wildcard, so `or-…` never matches.
        let c = Classifier::default();
        for (model, expected) in [
            ("claude-opus-5", BackendGroup::Claude),
            ("opus", BackendGroup::Claude),
            ("fable", BackendGroup::Claude),
            ("gpt-5.6-sol", BackendGroup::Codex),
            ("o3-mini", BackendGroup::Codex),
            ("o1", BackendGroup::Codex),
            ("sol", BackendGroup::Codex),
            ("grok-4.6", BackendGroup::Grok),
            ("grok", BackendGroup::Grok),
        ] {
            assert_eq!(
                c.classify(Some(model)),
                expected,
                "{model} must NOT be captured by the openrouter rules"
            );
        }
        // And the reverse direction: an `or-` id is claimed by nobody else,
        // which is what makes the check order in `classify` irrelevant here.
        assert_eq!(c.classify(Some("or-ox-alpha")), BackendGroup::OpenRouter);
    }

    #[test]
    fn config_openrouter_list_replaces_builtin() {
        let c = Classifier::from_config(&[], &[], &[], &["free-".to_string()], "claude");
        assert_eq!(c.classify(Some("free-model-1")), BackendGroup::OpenRouter);
        assert_eq!(
            c.classify(Some("or-ox-alpha")),
            BackendGroup::Claude,
            "builtin or- prefix dropped when config provides its own openrouter list"
        );
    }

    #[test]
    fn openrouter_can_be_the_default_group() {
        let c = Classifier::from_config(&[], &[], &[], &[], "openrouter");
        assert_eq!(c.classify(None), BackendGroup::OpenRouter);
        assert_eq!(
            c.classify(Some("totally-unknown")),
            BackendGroup::OpenRouter
        );
        // A recognized family still wins over the default.
        assert_eq!(c.classify(Some("claude-opus-5")), BackendGroup::Claude);
    }

    #[test]
    fn c2_config_grok_list_replaces_builtin() {
        let c = Classifier::from_config(&[], &[], &["mega-".to_string()], &[], "claude");
        assert_eq!(c.classify(Some("mega-1")), BackendGroup::Grok);
        assert_eq!(
            c.classify(Some("grok-4.5")),
            BackendGroup::Claude,
            "builtin grok prefix dropped when config provides its own grok list"
        );
    }

    #[test]
    fn c2_default_group_grok_parses() {
        let c = Classifier::from_config(&[], &[], &[], &[], "grok");
        assert_eq!(c.classify(None), BackendGroup::Grok);
    }

    #[test]
    fn config_codex_list_replaces_builtin() {
        // Config says ONLY "wizard-" is codex; gpt-5.5 is no longer codex.
        let c = Classifier::from_config(&[], &["wizard-".to_string()], &[], &[], "claude");
        assert_eq!(c.classify(Some("wizard-7b")), BackendGroup::Codex);
        assert_eq!(
            c.classify(Some("gpt-5.5")),
            BackendGroup::Claude,
            "builtin gpt- rule dropped when config provides its own codex list"
        );
    }

    #[test]
    fn config_claude_list_replaces_builtin() {
        let c = Classifier::from_config(&["acme-".to_string()], &[], &[], &[], "claude");
        assert_eq!(c.classify(Some("acme-1")), BackendGroup::Claude);
        // opus is no longer a claude model under the override; with no codex
        // match it falls back to the default group (claude).
        assert_eq!(c.classify(Some("opus")), BackendGroup::Claude);
        // gpt-5.5 still matches the builtin codex list (codex list empty →
        // builtins kept).
        assert_eq!(c.classify(Some("gpt-5.5")), BackendGroup::Codex);
    }

    #[test]
    fn config_can_move_a_model_across_groups() {
        // Make "opus" a CODEX model via config — config override wins over
        // the builtin claude prefix.
        let c = Classifier::from_config(&[], &["=opus".to_string()], &[], &[], "claude");
        assert_eq!(c.classify(Some("opus")), BackendGroup::Codex);
    }

    #[test]
    fn config_substring_and_exact_tokens_parse() {
        let c = Classifier::from_config(
            &[],
            &["~special".to_string(), "=exact-model".to_string()],
            &[],
            &[],
            "claude",
        );
        assert_eq!(c.classify(Some("my-special-build")), BackendGroup::Codex);
        assert_eq!(c.classify(Some("exact-model")), BackendGroup::Codex);
        assert_eq!(
            c.classify(Some("exact-model-2")),
            BackendGroup::Claude,
            "exact rule does not match a longer string"
        );
    }

    #[test]
    fn config_default_group_codex_changes_fallback() {
        let c = Classifier::from_config(&[], &[], &[], &[], "codex");
        assert_eq!(
            c.classify(None),
            BackendGroup::Codex,
            "absent model routes to the configured default"
        );
        assert_eq!(
            c.classify(Some("llama-3")),
            BackendGroup::Codex,
            "unknown model routes to the configured default"
        );
        // Explicit matches still win over the default.
        assert_eq!(c.classify(Some("opus")), BackendGroup::Claude);
    }

    // ---- model_from_body ----

    #[test]
    fn model_from_body_extracts_string_model() {
        assert_eq!(
            model_from_body(br#"{"model":"gpt-5.5","messages":[]}"#).as_deref(),
            Some("gpt-5.5")
        );
    }

    #[test]
    fn model_from_body_tolerates_missing_or_non_string_model() {
        assert_eq!(model_from_body(br#"{"messages":[]}"#), None);
        assert_eq!(model_from_body(br#"{"model":123}"#), None);
    }

    #[test]
    fn model_from_body_tolerates_non_json() {
        assert_eq!(model_from_body(b"not json at all"), None);
        assert_eq!(model_from_body(b""), None);
    }

    // ---- is_fable_model (central Fable classifier, fable-usage W2) ----

    #[test]
    fn is_fable_model_matches_fable_family_case_insensitively() {
        assert!(is_fable_model(Some("fable-5")));
        assert!(is_fable_model(Some("Fable")));
        assert!(is_fable_model(Some("FABLE-5-20251001")));
        // Vendor-prefixed variant is still recognized (contains, not prefix).
        assert!(is_fable_model(Some("claude-fable-5")));
        // The 5.1 generation and its `[1m]` catalog id (2026-09-02 roll).
        assert!(is_fable_model(Some("claude-fable-5-1")));
        assert!(is_fable_model(Some("claude-fable-5-1[1m]")));
    }

    #[test]
    fn is_fable_model_rejects_non_fable_and_absent_models() {
        assert!(!is_fable_model(Some("claude-haiku-4-5-20251001")));
        assert!(!is_fable_model(Some("claude-sonnet-4-5")));
        assert!(!is_fable_model(Some("gpt-5.5")));
        assert!(!is_fable_model(Some("")));
        // No model in the body must NOT be treated as Fable-scoped.
        assert!(!is_fable_model(None));
    }

    #[test]
    fn fable_scope_label_is_the_upstream_display_name() {
        // The label used to key Fable-scoped cooldowns must equal the usage
        // poll's `scope.model.display_name` (`.prd/13` §Carrier 1).
        assert_eq!(FABLE_SCOPE_LABEL, "Fable");
    }

    // ---- user_id_from_body (issue #32 metering identity) ----

    #[test]
    fn user_id_from_body_extracts_metadata_user_id() {
        assert_eq!(
            user_id_from_body(br#"{"model":"x","metadata":{"user_id":"acct_abc"}}"#).as_deref(),
            Some("acct_abc")
        );
    }

    #[test]
    fn user_id_from_body_tolerates_missing_metadata_or_user_id() {
        // No metadata block at all.
        assert_eq!(user_id_from_body(br#"{"model":"x"}"#), None);
        // metadata present but no user_id.
        assert_eq!(user_id_from_body(br#"{"metadata":{"foo":"bar"}}"#), None);
        // user_id present but not a string.
        assert_eq!(user_id_from_body(br#"{"metadata":{"user_id":42}}"#), None);
    }

    #[test]
    fn user_id_from_body_tolerates_non_json() {
        assert_eq!(user_id_from_body(b"not json at all"), None);
        assert_eq!(user_id_from_body(b""), None);
    }
}
