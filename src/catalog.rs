//! Model catalog served by `GET /models` and `GET /llmux/models`.
//!
//! Descriptive metadata — the ids, human names, reasoning-effort menus, and
//! context windows of the KNOWN models: a curated set plus the live grok pin.
//! This is NOT an exhaustive list of everything routable: at request time the
//! grok provider passes ANY `grok-*` id through verbatim, so a config that
//! pins an id outside the curated set still works — it simply appears here as
//! a synthesized row with null metadata (see [`catalog`]). This module carries
//! no request-shaping/acceptance logic (that lives per provider). One dynamic
//! bit: the grok family alias `"grok"` attaches to whichever grok id is the
//! live pin, mirroring [`crate::provider::grok`]'s bare-`grok` routing.
//!
//! Sources (evidence gathered 2026-07-14; claude rows re-curated 2026-07-27):
//! - Claude rows: user-curated 2026-07-27, and they live in [`CLAUDE_MODELS`]
//!   — that const is the SSOT for both these rows and the alias→id resolution
//!   in [`crate::provider::anthropic`] (Claude Code model picker; `[1m]`
//!   suffix marks the 1M-context variant ids). The effort menus are the Claude
//!   Code `/effort` levels, applied per the user contract — llmux itself does
//!   NOT shape claude request PARAMETERS (effort/thinking are client metadata,
//!   advertised here and passed through). It DOES normalize the outbound
//!   `model` field: [`crate::provider::anthropic`] resolves a curated alias to
//!   its catalog id and strips the `[1m]` context suffix, with
//!   [`CLAUDE_MODELS`] as the mapping source.
//! - Codex context windows and effort menus: the openai/codex model catalog
//!   (`models-manager/models.json`), fetched 2026-07-14. `gpt-5.6-sol/terra`
//!   support low..ultra; `gpt-5.6-luna` low..max; `gpt-5.5` low..xhigh. The
//!   `gpt-5.6-*[1m]` rows are the codex side of the `[1m]` opt-in and carry
//!   OpenAI's published 1,050,000-token family window rather than the
//!   catalog's 372,000; live probes 2026-08-21 against the ChatGPT-account
//!   backend corroborate it on SOL specifically (910,229 accepted, ~936k
//!   rejected) and reach only 555,029 accepted on terra.
//! - Grok context windows / names: the live `cli-chat-proxy` `/v1/models`
//!   probe — `grok-4.5` ctx 500000 (2026-07-14), `grok-4.6` ctx 500000, name
//!   "Grok 4.6" (2026-08-13). Grok effort menus come from
//!   [`crate::provider::grok::thinking_levels_catalog`] (models with no
//!   thinking support get an empty menu). The curated grok set is `grok-4.6`
//!   plus `grok-4.5`; any other `grok-*` id passes through at request time and
//!   synthesizes a null-metadata row when pinned.

use std::borrow::Cow;

use serde::Serialize;

/// One catalog row. Field order is the serialized JSON key order:
/// `id, aliases, name, efforts, max_context, group`. `id`/`name` are `Cow`
/// so curated rows stay zero-alloc `&'static str` while a synthesized
/// out-of-catalog pin row can own its slug.
#[derive(Debug, Clone, Serialize)]
pub struct ModelEntry {
    /// Concrete upstream model id.
    pub id: Cow<'static, str>,
    /// Extra request slugs that resolve to this id (family/variant aliases).
    pub aliases: Vec<String>,
    /// Human-facing display name.
    pub name: Cow<'static, str>,
    /// Accepted `reasoning.effort` values, low→high; empty when the model
    /// takes no reasoning field.
    pub efforts: &'static [&'static str],
    /// Context window in tokens, or `None` when unpublished.
    pub max_context: Option<u64>,
    /// Backend group: `claude`, `codex`, `grok`, or `openrouter`.
    pub group: &'static str,
}

/// Claude effort menu — the Claude Code `/effort` levels, per the user
/// contract (llmux shapes no claude request PARAMETERS, only the outbound
/// `model`; these values are client metadata).
const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

// ---- codex effort menus (openai/codex models.json, 2026-07-14) ----
const CODEX_EFFORTS_SOL_TERRA: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];
const CODEX_EFFORTS_LUNA: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const CODEX_EFFORTS_GPT55: &[&str] = &["low", "medium", "high", "xhigh"];

/// The curated claude catalog — SSOT for BOTH the `/models` rows below and the
/// alias→id resolution in [`crate::provider::anthropic`]. Before this table
/// existed the aliases were advertised by `/models` but never honored upstream
/// (a bare `opus` was forwarded verbatim and 404'd at api.anthropic.com).
/// Adding a model = one row here; the alias follows automatically.
/// Tuple = (id, aliases, display name, max_context).
pub(crate) const CLAUDE_MODELS: &[(&str, &[&str], &str, u64)] = &[
    (
        "claude-fable-5[1m]",
        &["fable"],
        "Claude Fable 5",
        1_000_000,
    ),
    (
        "claude-opus-5[1m]",
        &["opus", "opus-5"],
        "Claude Opus 5 [1M]",
        1_000_000,
    ),
    ("claude-opus-5", &[], "Claude Opus 5", 200_000),
    ("claude-opus-4-8[1m]", &[], "Claude Opus 4.8", 1_000_000),
    ("claude-opus-4-6[1m]", &[], "Claude Opus 4.6", 1_000_000),
    (
        "claude-sonnet-5[1m]",
        &["sonnet", "sonnet-5"],
        "Claude Sonnet 5 [1M]",
        1_000_000,
    ),
    ("claude-sonnet-5", &[], "Claude Sonnet 5", 200_000),
    ("claude-haiku-4-5", &["haiku"], "Claude Haiku 4.5", 200_000),
];

/// The curated grok catalog, newest first. Tuple = (id, display name,
/// max_context); effort menus are looked up per id in
/// [`crate::provider::grok::thinking_levels_catalog`] rather than duplicated
/// here. The `"grok"` family alias is NOT in this table — it belongs to
/// whichever row is the live pin (see [`catalog`]).
const GROK_MODELS: &[(&str, &str, u64)] = &[
    ("grok-4.6", "Grok 4.6", 500_000),
    ("grok-4.5", "Grok 4.5", 500_000),
];

// ---- openrouter effort menus (live `GET /api/v1/models`
//      `reasoning.supported_efforts`, 2026-08-21, re-sorted low→high) ----
const OR_EFFORTS_OX: &[&str] = &["low", "high", "max"];
const OR_EFFORTS_GLM: &[&str] = &["high", "xhigh"];
const OR_EFFORTS_NEMOTRON_ULTRA: &[&str] = &["medium", "high"];
const OR_EFFORTS_GPT_OSS: &[&str] = &["low", "medium", "high"];
const OR_EFFORTS_NONE: &[&str] = &[];

/// The curated OpenRouter catalog — SSOT for BOTH the `/models` rows and the
/// `or-…` → upstream-slug resolution in [`crate::provider::openrouter`].
///
/// Column 0 is the id llmux ADVERTISES and the client SENDS (`or-ox-alpha`);
/// column 1 is the OpenRouter slug it is rewritten to on the wire
/// (`stealth/ox-alpha`). They differ on purpose: the advertised id must match
/// the `or-` routing rule (docs/openrouter/spec.md §R2) so Claude Code's model
/// picker produces something that actually routes to this group, while the
/// wire slug is whatever OpenRouter calls the model.
///
/// This table is a CONVENIENCE layer, not a gate — `or-<vendor>/<slug>` passes
/// through verbatim for the ~400 models not listed here (spec §R3 step 3).
///
/// Tuple = (advertised id, upstream slug, display name, max_context, efforts).
/// Every value is from the live `GET /api/v1/models` probe on 2026-08-21.
pub(crate) const OPENROUTER_MODELS: &[(&str, &str, &str, u64, &[&str])] = &[
    (
        "or-ox-alpha",
        "stealth/ox-alpha",
        "Ox Alpha (free)",
        1_048_576,
        OR_EFFORTS_OX,
    ),
    (
        "or-free",
        "openrouter/free",
        "OpenRouter Free Models Router",
        200_000,
        OR_EFFORTS_NONE,
    ),
    (
        "or-glm-5.2",
        "z-ai/glm-5.2:free",
        "Z.ai GLM 5.2 (free)",
        256_000,
        OR_EFFORTS_GLM,
    ),
    (
        "or-nemotron-3-ultra",
        "nvidia/nemotron-3-ultra-550b-a55b:free",
        "NVIDIA Nemotron 3 Ultra (free)",
        1_000_000,
        OR_EFFORTS_NEMOTRON_ULTRA,
    ),
    (
        "or-nemotron-3.5-lightning",
        "nvidia/nemotron-3.5-lightning:free",
        "NVIDIA Nemotron 3.5 Lightning (free)",
        1_000_000,
        OR_EFFORTS_NONE,
    ),
    (
        "or-dots-3-note",
        "dots-studio/dots-3-note-preview:free",
        "Dots3-Note Preview (free)",
        512_000,
        OR_EFFORTS_NONE,
    ),
    (
        "or-laguna-s-2.1",
        "poolside/laguna-s-2.1:free",
        "Poolside Laguna S 2.1 (free)",
        262_144,
        OR_EFFORTS_NONE,
    ),
    (
        "or-north-mini-code",
        "cohere/north-mini-code:free",
        "Cohere North Mini Code (free)",
        256_000,
        OR_EFFORTS_NONE,
    ),
    (
        "or-gemma-4-31b",
        "google/gemma-4-31b-it:free",
        "Google Gemma 4 31B (free)",
        262_144,
        OR_EFFORTS_NONE,
    ),
    (
        "or-gpt-oss-20b",
        "openai/gpt-oss-20b:free",
        "OpenAI gpt-oss-20b (free)",
        131_072,
        OR_EFFORTS_GPT_OSS,
    ),
];

/// The default OpenRouter pin — the wire slug the bare `or` alias resolves to.
/// SSOT for BOTH `config::schema::default_openrouter_model` (what a fresh
/// config writes) and `provider::openrouter::OPENROUTER_DEFAULT_MODEL` (what
/// the provider exports); it lives here because [`OPENROUTER_MODELS`] is
/// already the model SSOT and two literal copies would drift silently — a
/// config advertising one pin while the provider resolved another.
pub(crate) const OPENROUTER_DEFAULT_PIN: &str = "stealth/ox-alpha";

/// Resolve a curated OpenRouter advertised id (`or-ox-alpha`) to its upstream
/// slug (`stealth/ox-alpha`), or `None` when it is not curated — the provider
/// then applies the verbatim-passthrough rules of spec §R3.
///
/// Trimmed and ASCII-lowercased before matching, matching the classifier's
/// normalization so an id that ROUTES here also RESOLVES here.
pub(crate) fn resolve_openrouter_alias(model: &str) -> Option<&'static str> {
    let needle = model.trim().to_ascii_lowercase();
    OPENROUTER_MODELS
        .iter()
        .find(|(id, ..)| *id == needle.as_str())
        .map(|&(_, slug, ..)| slug)
}

/// Resolve a curated claude ALIAS (`opus`, `opus-5`, `sonnet`, `haiku`, …) to
/// its catalog id, or `None` when `model` is not an alias (a real id, or any
/// foreign slug — those must pass through untouched). Trimmed and
/// ASCII-lowercased before matching. [`CLAUDE_MODELS`] is the only mapping
/// source, so a new curated row carries its aliases automatically.
///
/// Two consumers must agree on this, which is why it lives here rather than in
/// either of them: `provider::anthropic` rewrites the outbound `model` so the
/// alias `/models` advertises is honored upstream, and `tui::activity`'s
/// `normalize_model` applies the same mapping so usage and pricing are booked
/// against the id that actually served the request.
pub(crate) fn resolve_claude_alias(model: &str) -> Option<&'static str> {
    let needle = model.trim().to_ascii_lowercase();
    CLAUDE_MODELS
        .iter()
        .find(|(_, aliases, _, _)| aliases.contains(&needle.as_str()))
        .map(|&(id, _, _, _)| id)
}

/// The known model catalog in canonical group order (`claude < codex < grok`).
/// `grok_pin` / `codex_pin` are the live provider model slugs; the entry whose
/// id equals `grok_pin` additionally advertises the `"grok"` family alias.
///
/// The curated grok set is [`GROK_MODELS`] (`grok-4.6`, `grok-4.5`); when
/// `grok_pin` matches NEITHER curated id (a config can pin ANY `grok-*` slug,
/// e.g. `grok-code-fast-1`), a synthesized row is appended after the curated
/// grok entries so the `"grok"` alias always has an owner: id = the pin, name =
/// the pin verbatim, efforts from the thinking-level lookup (else empty),
/// context null. `codex_pin` is unused for aliasing (a model-less codex request
/// uses the pin directly, which is not an alias) — accepted for symmetry /
/// future use.
pub fn catalog(grok_pin: &str, _codex_pin: &str, openrouter_pin: &str) -> Vec<ModelEntry> {
    let mut entries = Vec::new();

    // ---- claude (user-curated 2026-07-27) ----
    for &(id, aliases, name, ctx) in CLAUDE_MODELS {
        entries.push(ModelEntry {
            id: Cow::Borrowed(id),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            name: Cow::Borrowed(name),
            efforts: CLAUDE_EFFORTS,
            max_context: Some(ctx),
            group: "claude",
        });
    }

    // ---- codex ----
    // The `[1m]` rows mirror the claude convention: a client-side
    // context-denominator opt-in that the provider strips before the request
    // leaves llmux. 1_000_000 is advertised on the strength of OpenAI's
    // PUBLISHED 1,050,000 total window for the gpt-5.6 family; the 2026-08-21
    // probes against the ChatGPT-account backend corroborate it but do not by
    // themselves establish it per model. Those probes are sol-specific where
    // they are strong — `gpt-5.6-sol` accepted 910,229 input tokens and was
    // rejected at ~936k ("Your input exceeds the context window of this
    // model") — while `gpt-5.6-terra` was only probed to 555,029 accepted,
    // with no upper bound found; terra rides the family figure. The base rows
    // keep the openai/codex catalog's 372,000 — the window a client gets
    // without opting in — exactly as the claude base rows keep 200,000 next to
    // their `[1m]` twins. No `gpt-5.6-luna[1m]` (luna still 404s upstream) and
    // no `gpt-5.5[1m]` (272k family). Aliases stay on the base rows — a suffix
    // is an explicit opt-in, never something an alias silently picks.
    entries.push(codex_entry(
        "gpt-5.6-sol[1m]",
        "GPT-5.6-Sol [1M]",
        CODEX_EFFORTS_SOL_TERRA,
        Some(1_000_000),
        Vec::new(),
    ));
    entries.push(codex_entry(
        "gpt-5.6-sol",
        "GPT-5.6-Sol",
        CODEX_EFFORTS_SOL_TERRA,
        Some(372_000),
        vec!["sol".into(), "gpt-5.6".into()],
    ));
    entries.push(codex_entry(
        "gpt-5.6-terra[1m]",
        "GPT-5.6-Terra [1M]",
        CODEX_EFFORTS_SOL_TERRA,
        Some(1_000_000),
        Vec::new(),
    ));
    entries.push(codex_entry(
        "gpt-5.6-terra",
        "GPT-5.6-Terra",
        CODEX_EFFORTS_SOL_TERRA,
        Some(372_000),
        vec!["terra".into()],
    ));
    entries.push(codex_entry(
        "gpt-5.6-luna",
        "GPT-5.6-Luna",
        CODEX_EFFORTS_LUNA,
        Some(372_000),
        vec!["luna".into()],
    ));
    entries.push(codex_entry(
        "gpt-5.5",
        "GPT-5.5",
        CODEX_EFFORTS_GPT55,
        Some(272_000),
        Vec::new(),
    ));

    // ---- grok (curated) ----
    // The `"grok"` alias rides the pin: exactly the curated row whose id IS
    // `grok_pin` carries it, so at most one curated row owns it.
    let mut pin_owned = false;
    for &(id, name, ctx) in GROK_MODELS {
        let owns_alias = id == grok_pin;
        pin_owned |= owns_alias;
        entries.push(ModelEntry {
            id: Cow::Borrowed(id),
            aliases: if owns_alias {
                vec!["grok".to_string()]
            } else {
                Vec::new()
            },
            name: Cow::Borrowed(name),
            efforts: grok_efforts(id),
            max_context: Some(ctx),
            group: "grok",
        });
    }

    // ---- grok (synthesized out-of-catalog pin) ----
    // The provider forwards any `grok-*` id verbatim, so a pin outside the
    // curated set is real and routable — give the `"grok"` alias an owner
    // rather than orphaning it. Metadata is null (unknown), effort menu from
    // the thinking-level lookup if the id happens to be a known reasoner.
    if !pin_owned {
        entries.push(ModelEntry {
            id: Cow::Owned(grok_pin.to_string()),
            aliases: vec!["grok".to_string()],
            name: Cow::Owned(grok_pin.to_string()),
            efforts: grok_efforts(grok_pin),
            max_context: None,
            group: "grok",
        });
    }

    // ---- openrouter (curated free set, live probe 2026-08-21) ----
    // The bare `"or"` family alias rides the pin exactly like `"grok"` does:
    // the curated row whose UPSTREAM SLUG equals `openrouter_pin` carries it,
    // so at most one row owns it. When the pin is outside the curated set a
    // synthesized row is appended so the alias is never orphaned.
    let mut or_pin_owned = false;
    for &(id, slug, name, ctx, efforts) in OPENROUTER_MODELS {
        let owns_alias = slug == openrouter_pin;
        or_pin_owned |= owns_alias;
        entries.push(ModelEntry {
            id: Cow::Borrowed(id),
            aliases: if owns_alias {
                vec!["or".to_string()]
            } else {
                Vec::new()
            },
            name: Cow::Borrowed(name),
            efforts,
            max_context: Some(ctx),
            group: "openrouter",
        });
    }
    if !or_pin_owned {
        // A config can pin ANY OpenRouter slug; advertise it under the id a
        // client can actually type (`or-<slug>`), metadata unknown.
        entries.push(ModelEntry {
            id: Cow::Owned(format!("or-{openrouter_pin}")),
            aliases: vec!["or".to_string()],
            name: Cow::Owned(openrouter_pin.to_string()),
            efforts: OR_EFFORTS_NONE,
            max_context: None,
            group: "openrouter",
        });
    }

    entries
}

fn codex_entry(
    id: &'static str,
    name: &'static str,
    efforts: &'static [&'static str],
    max_context: Option<u64>,
    aliases: Vec<String>,
) -> ModelEntry {
    ModelEntry {
        id: Cow::Borrowed(id),
        aliases,
        name: Cow::Borrowed(name),
        efforts,
        max_context,
        group: "codex",
    }
}

/// Grok effort menu for `id`, from the provider's thinking-level table; models
/// with no thinking support get an empty menu.
fn grok_efforts(id: &str) -> &'static [&'static str] {
    crate::provider::grok::thinking_levels_catalog()
        .iter()
        .find(|(model, _)| *model == id)
        .map(|(_, levels)| *levels)
        .unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(entries: &'a [ModelEntry], id: &str) -> &'a ModelEntry {
        entries
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("entry {id} present"))
    }

    #[test]
    fn catalog_matches_user_contract_26_entries() {
        // The pinned (curated) case: exactly 26 rows, claude ids in order.
        // 14 before the codex `[1m]` pair landed (2026-08-21); 16 before the
        // 10 curated openrouter free rows landed (2026-08-21).
        let entries = catalog("grok-4.6", "gpt-5.6-sol", "stealth/ox-alpha");
        assert_eq!(entries.len(), 26);
        let claude_ids: Vec<&str> = entries
            .iter()
            .filter(|e| e.group == "claude")
            .map(|e| e.id.as_ref())
            .collect();
        assert_eq!(
            claude_ids,
            vec![
                "claude-fable-5[1m]",
                "claude-opus-5[1m]",
                "claude-opus-5",
                "claude-opus-4-8[1m]",
                "claude-opus-4-6[1m]",
                "claude-sonnet-5[1m]",
                "claude-sonnet-5",
                "claude-haiku-4-5",
            ]
        );
    }

    #[test]
    fn catalog_groups_in_canonical_order() {
        let entries = catalog("grok-4.6", "gpt-5.6-sol", "stealth/ox-alpha");
        let groups: Vec<&str> = entries.iter().map(|e| e.group).collect();
        let first_codex = groups.iter().position(|g| *g == "codex").unwrap();
        let first_grok = groups.iter().position(|g| *g == "grok").unwrap();
        let first_or = groups.iter().position(|g| *g == "openrouter").unwrap();
        assert!(groups[..first_codex].iter().all(|g| *g == "claude"));
        assert!(groups[first_codex..first_grok]
            .iter()
            .all(|g| *g == "codex"));
        assert!(groups[first_grok..first_or].iter().all(|g| *g == "grok"));
        assert!(groups[first_or..].iter().all(|g| *g == "openrouter"));
    }

    #[test]
    fn claude_entries_carry_curated_efforts_and_context() {
        let entries = catalog("grok-4.6", "gpt-5.6-sol", "stealth/ox-alpha");
        for e in entries.iter().filter(|e| e.group == "claude") {
            assert_eq!(
                e.efforts,
                &["low", "medium", "high", "xhigh", "max"],
                "{}: Claude Code effort levels",
                e.id
            );
        }
        // Curated aliases and 1M-vs-standard context windows.
        assert_eq!(find(&entries, "claude-fable-5[1m]").aliases, vec!["fable"]);
        assert_eq!(
            find(&entries, "claude-opus-5[1m]").aliases,
            vec!["opus", "opus-5"]
        );
        assert!(find(&entries, "claude-opus-5").aliases.is_empty());
        // The `opus` alias MOVED off 4.8 onto opus-5 — this emptiness is the
        // regression this test guards (a stale alias would keep bare `opus`
        // resolving to the old model).
        assert!(find(&entries, "claude-opus-4-8[1m]").aliases.is_empty());
        assert!(find(&entries, "claude-opus-4-6[1m]").aliases.is_empty());
        assert_eq!(
            find(&entries, "claude-sonnet-5[1m]").aliases,
            vec!["sonnet", "sonnet-5"]
        );
        assert!(find(&entries, "claude-sonnet-5").aliases.is_empty());
        assert_eq!(find(&entries, "claude-haiku-4-5").aliases, vec!["haiku"]);
        assert_eq!(
            find(&entries, "claude-opus-5[1m]").max_context,
            Some(1_000_000)
        );
        assert_eq!(find(&entries, "claude-opus-5").max_context, Some(200_000));
        assert_eq!(
            find(&entries, "claude-sonnet-5[1m]").max_context,
            Some(1_000_000)
        );
        assert_eq!(find(&entries, "claude-sonnet-5").max_context, Some(200_000));
    }

    /// `CLAUDE_MODELS` is now ALSO the alias→id resolution source for
    /// [`crate::provider::anthropic`], so a duplicated alias would silently
    /// make one row unreachable (first/last match wins, depending on the
    /// resolver's scan order) — the table must keep aliases globally unique.
    #[test]
    fn claude_aliases_are_unique_across_rows() {
        let mut aliases: Vec<&str> = CLAUDE_MODELS
            .iter()
            .flat_map(|(_, aliases, _, _)| aliases.iter().copied())
            .collect();
        aliases.sort_unstable();
        for pair in aliases.windows(2) {
            assert_ne!(pair[0], pair[1], "duplicate claude alias {:?}", pair[0]);
        }
    }

    /// The companion invariant to alias uniqueness: no curated ID may ALSO be
    /// an alias. `resolve_claude_alias` deliberately returns `None` for a real
    /// id (the `[1m]` strip is a separate downstream step), and
    /// `provider::anthropic`'s `resolve_client_alias` rewrites the outbound
    /// `model` from whatever it returns — so an id captured as some other
    /// row's alias would turn an explicitly chosen model into a different one.
    /// The failure mode is SILENT model substitution, not an error, which is
    /// why it is pinned here rather than left to review.
    #[test]
    fn no_curated_id_is_also_an_alias() {
        for &(id, _, _, _) in CLAUDE_MODELS {
            assert_eq!(
                resolve_claude_alias(id),
                None,
                "curated id {id:?} is also an alias — requests for it would be \
                 silently rewritten to {:?}",
                resolve_claude_alias(id)
            );
        }
    }

    /// Table-driven: every alias of every curated row resolves to that row's
    /// id. Adding a row cannot leave this test stale — it iterates the SSOT.
    #[test]
    fn resolve_claude_alias_maps_every_curated_alias_to_its_row() {
        for &(id, aliases, _, _) in CLAUDE_MODELS {
            for alias in aliases {
                assert_eq!(
                    resolve_claude_alias(alias),
                    Some(id),
                    "alias {alias:?} must resolve to {id}"
                );
            }
        }
    }

    #[test]
    fn resolve_claude_alias_is_trimmed_and_case_insensitive() {
        assert_eq!(resolve_claude_alias("  OPUS  "), Some("claude-opus-5[1m]"));
    }

    /// Deliberate asymmetry: a real id is NOT an alias (the `[1m]` strip is a
    /// separate downstream step), and foreign slugs must pass through
    /// untouched so non-claude routing is never rewritten.
    #[test]
    fn resolve_claude_alias_rejects_ids_and_foreign_slugs() {
        for slug in [
            "claude-opus-5",
            "claude-opus-5[1m]",
            "claude-opus-4-8[1m]",
            "grok-4.6",
            "grok-4.5",
            "gpt-5.6-sol",
            "",
        ] {
            assert_eq!(resolve_claude_alias(slug), None, "{slug:?} is not an alias");
        }
    }

    #[test]
    fn curated_grok_rows_carry_context_and_efforts() {
        let entries = catalog("grok-4.6", "gpt-5.6-sol", "stealth/ox-alpha");
        // grok-4.6: live /v1/models probe 2026-08-13 (ctx 500000).
        let g46 = find(&entries, "grok-4.6");
        assert_eq!(g46.name, "Grok 4.6");
        assert_eq!(g46.max_context, Some(500_000));
        assert_eq!(g46.efforts, &["low", "medium", "high", "xhigh"]);
        // grok-4.5 stays curated with its 2026-07-14 metadata.
        let g45 = find(&entries, "grok-4.5");
        assert_eq!(g45.name, "Grok 4.5");
        assert_eq!(g45.max_context, Some(500_000));
        assert_eq!(g45.efforts, &["low", "medium", "high"]);
    }

    #[test]
    fn grok_family_alias_follows_the_pin() {
        // Default pin: the newest curated row owns the alias.
        let pinned = catalog("grok-4.6", "gpt-5.6-sol", "stealth/ox-alpha");
        assert_eq!(find(&pinned, "grok-4.6").aliases, vec!["grok".to_string()]);
        assert!(find(&pinned, "grok-4.5").aliases.is_empty());

        // The older curated row can be pinned too — the alias moves to it.
        let pinned = catalog("grok-4.5", "gpt-5.6-sol", "stealth/ox-alpha");
        assert_eq!(find(&pinned, "grok-4.5").aliases, vec!["grok".to_string()]);
        assert!(find(&pinned, "grok-4.6").aliases.is_empty());

        // Out-of-catalog pin: alias moves to the synthesized row, and NEITHER
        // curated row keeps it.
        let pinned = catalog("grok-4.3", "gpt-5.6-sol", "stealth/ox-alpha");
        assert!(find(&pinned, "grok-4.6").aliases.is_empty());
        assert!(find(&pinned, "grok-4.5").aliases.is_empty());
        assert_eq!(find(&pinned, "grok-4.3").aliases, vec!["grok".to_string()]);
    }

    #[test]
    fn in_catalog_pin_does_not_synthesize_a_row() {
        // A curated pin: no synthesized row, alias on the static row, count 26.
        for (pin, owner) in [("grok-4.6", "grok-4.6"), ("grok-4.5", "grok-4.5")] {
            let entries = catalog(pin, "gpt-5.6-sol", "stealth/ox-alpha");
            assert_eq!(entries.len(), 26, "pin {pin}");
            let owners: Vec<&str> = entries
                .iter()
                .filter(|e| e.aliases.iter().any(|a| a == "grok"))
                .map(|e| e.id.as_ref())
                .collect();
            assert_eq!(owners, vec![owner], "pin {pin}");
        }
    }

    #[test]
    fn out_of_catalog_pin_synthesizes_a_null_metadata_row() {
        // A pin outside the curated set (routable via provider passthrough)
        // gets exactly one synthesized owner of the "grok" alias.
        let entries = catalog("grok-code-fast-1", "gpt-5.6-sol", "stealth/ox-alpha");
        assert_eq!(entries.len(), 27);
        let owners: Vec<&ModelEntry> = entries
            .iter()
            .filter(|e| e.aliases.iter().any(|a| a == "grok"))
            .collect();
        assert_eq!(owners.len(), 1, "exactly one owner of the grok alias");
        let synth = owners[0];
        assert_eq!(synth.id, "grok-code-fast-1");
        assert_eq!(synth.name, "grok-code-fast-1");
        assert_eq!(synth.max_context, None);
        assert_eq!(synth.efforts, &[] as &[&str], "unknown id → no efforts");
        assert_eq!(synth.group, "grok");
        // Appended after the curated grok rows — i.e. the LAST grok row (the
        // openrouter block follows the whole grok block, so this is no longer
        // the last entry of the catalog).
        let last_grok = entries
            .iter()
            .rfind(|e| e.group == "grok")
            .expect("at least one grok row");
        assert_eq!(last_grok.id, "grok-code-fast-1");
    }

    #[test]
    fn synthesized_pin_keeps_known_thinking_levels() {
        // A known reasoner pinned outside the curated set still gets its effort
        // menu from the thinking-level lookup, even though metadata is null.
        let entries = catalog("grok-4.3", "gpt-5.6-sol", "stealth/ox-alpha");
        assert_eq!(entries.len(), 27);
        // Both curated rows survive an out-of-catalog pin.
        assert_eq!(find(&entries, "grok-4.6").max_context, Some(500_000));
        assert_eq!(find(&entries, "grok-4.5").max_context, Some(500_000));
        let synth = find(&entries, "grok-4.3");
        assert_eq!(synth.efforts, &["none", "low", "medium", "high"]);
        assert_eq!(synth.max_context, None);
        assert_eq!(synth.aliases, vec!["grok".to_string()]);
    }

    #[test]
    fn gpt_5_6_sol_aliases_context_and_effort_count() {
        let entries = catalog("grok-4.6", "gpt-5.6-sol", "stealth/ox-alpha");
        let sol = find(&entries, "gpt-5.6-sol");
        assert_eq!(sol.aliases, vec!["sol".to_string(), "gpt-5.6".to_string()]);
        assert_eq!(sol.max_context, Some(372_000));
        assert_eq!(sol.efforts.len(), 6);
    }

    #[test]
    fn codex_1m_rows_advertise_the_probed_window_without_aliases() {
        // The `[1m]` opt-in advertises the same 1_000_000 the claude `[1m]`
        // rows do, on OpenAI's published 1,050,000 family window; the
        // 2026-08-21 probes corroborate it (sol: 910,229 accepted / ~936k
        // rejected; terra: 555,029 accepted, no ceiling found). The base rows
        // keep the openai/codex catalog's 372,000 (the non-opt-in
        // denominator).
        let entries = catalog("grok-4.6", "gpt-5.6-sol", "stealth/ox-alpha");
        for (id, name) in [
            ("gpt-5.6-sol[1m]", "GPT-5.6-Sol [1M]"),
            ("gpt-5.6-terra[1m]", "GPT-5.6-Terra [1M]"),
        ] {
            let e = find(&entries, id);
            assert_eq!(e.name, name);
            assert_eq!(e.max_context, Some(1_000_000), "{id}");
            assert_eq!(e.group, "codex", "{id}");
            assert_eq!(e.efforts, CODEX_EFFORTS_SOL_TERRA, "{id}");
            // Aliases stay on the base row — a suffix is an explicit opt-in.
            assert!(e.aliases.is_empty(), "{id} carries no alias");
        }
        assert_eq!(find(&entries, "gpt-5.6-sol").max_context, Some(372_000));
        assert_eq!(find(&entries, "gpt-5.6-terra").max_context, Some(372_000));
        // Not curated: luna still 404s upstream, gpt-5.5 is a 272k family.
        for absent in ["gpt-5.6-luna[1m]", "gpt-5.5[1m]"] {
            assert!(
                !entries.iter().any(|e| e.id == absent),
                "{absent} must not be curated"
            );
        }
    }

    #[test]
    fn dropped_ids_are_absent() {
        let entries = catalog("grok-4.6", "gpt-5.6-sol", "stealth/ox-alpha");
        for gone in [
            "gpt-5.5-codex",
            "gpt-5-codex",
            "grok-4.3",
            "grok-3-mini",
            "grok-build-0.1",
            "grok-composer-2.5-fast",
        ] {
            assert!(
                !entries.iter().any(|e| e.id == gone),
                "{gone} must not be curated"
            );
        }
    }

    #[test]
    fn entry_serializes_with_all_required_fields() {
        let entries = catalog("grok-4.5", "gpt-5.6-sol", "stealth/ox-alpha");
        let value = serde_json::to_value(find(&entries, "grok-4.5")).unwrap();
        let obj = value.as_object().unwrap();
        // Required keys present (additive evolution may add more).
        for key in ["id", "aliases", "name", "efforts", "max_context", "group"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }

        // The serialized string preserves struct field order for the required
        // fields (pinned so the wire contract stays stable).
        let json = serde_json::to_string(find(&entries, "grok-4.5")).unwrap();
        assert_eq!(
            json,
            r#"{"id":"grok-4.5","aliases":["grok"],"name":"Grok 4.5","efforts":["low","medium","high"],"max_context":500000,"group":"grok"}"#
        );

        // Same contract for the newer curated row, pinned so it owns the alias.
        let entries = catalog("grok-4.6", "gpt-5.6-sol", "stealth/ox-alpha");
        let json = serde_json::to_string(find(&entries, "grok-4.6")).unwrap();
        assert_eq!(
            json,
            r#"{"id":"grok-4.6","aliases":["grok"],"name":"Grok 4.6","efforts":["low","medium","high","xhigh"],"max_context":500000,"group":"grok"}"#
        );
    }
}
