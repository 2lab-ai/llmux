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
//! Sources (evidence gathered 2026-07-14):
//! - Claude rows: user-curated 2026-07-14 (Claude Code model picker; `[1m]`
//!   suffix marks the 1M-context variant ids). The effort menus are the Claude
//!   Code `/effort` levels, applied per the user contract — llmux itself does
//!   NOT shape claude requests (bare names still merely ROUTE to the group),
//!   these values are advertised metadata for clients.
//! - Codex context windows and effort menus: the openai/codex model catalog
//!   (`models-manager/models.json`), fetched 2026-07-14. `gpt-5.6-sol/terra`
//!   support low..ultra; `gpt-5.6-luna` low..max; `gpt-5.5` low..xhigh.
//! - Grok context window / name: the live `cli-chat-proxy` `/v1/models` probe
//!   2026-07-14 (`grok-4.5` ctx 500000). Grok effort menus come from
//!   [`crate::provider::grok::thinking_levels_catalog`] (models with no
//!   thinking support get an empty menu). The curated grok set is just
//!   `grok-4.5`; any other `grok-*` id passes through at request time and
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
    /// Backend group: `claude`, `codex`, or `grok`.
    pub group: &'static str,
}

/// Claude effort menu — the Claude Code `/effort` levels, per the user
/// contract (llmux does not shape claude requests; this is client metadata).
const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

// ---- codex effort menus (openai/codex models.json, 2026-07-14) ----
const CODEX_EFFORTS_SOL_TERRA: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];
const CODEX_EFFORTS_LUNA: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const CODEX_EFFORTS_GPT55: &[&str] = &["low", "medium", "high", "xhigh"];

/// The known model catalog in canonical group order (`claude < codex < grok`).
/// `grok_pin` / `codex_pin` are the live provider model slugs; the entry whose
/// id equals `grok_pin` additionally advertises the `"grok"` family alias.
///
/// The curated grok set is just `grok-4.5`; when `grok_pin` is anything else (a
/// config can pin ANY `grok-*` slug, e.g. `grok-code-fast-1`), a synthesized
/// row is appended after the curated grok entry so the `"grok"` alias always
/// has an owner: id = the pin, name = the pin verbatim, efforts from the
/// thinking-level lookup (else empty), context null. `codex_pin` is unused for
/// aliasing (a model-less codex request uses the pin directly, which is not an
/// alias) — accepted for symmetry / future use.
pub fn catalog(grok_pin: &str, _codex_pin: &str) -> Vec<ModelEntry> {
    let mut entries = Vec::new();

    // ---- claude (user-curated 2026-07-14) ----
    for (id, aliases, name, ctx) in [
        (
            "claude-fable-5[1m]",
            &["fable"][..],
            "Claude Fable 5",
            1_000_000u64,
        ),
        (
            "claude-opus-4-8[1m]",
            &["opus"][..],
            "Claude Opus 4.8",
            1_000_000,
        ),
        ("claude-opus-4-6[1m]", &[][..], "Claude Opus 4.6", 1_000_000),
        (
            "claude-sonnet-5[1m]",
            &["sonnet"][..],
            "Claude Sonnet 5 [1M]",
            1_000_000,
        ),
        ("claude-sonnet-5", &[][..], "Claude Sonnet 5", 200_000),
        (
            "claude-haiku-4-5",
            &["haiku"][..],
            "Claude Haiku 4.5",
            200_000,
        ),
    ] {
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
    entries.push(codex_entry(
        "gpt-5.6-sol",
        "GPT-5.6-Sol",
        CODEX_EFFORTS_SOL_TERRA,
        Some(372_000),
        vec!["sol".into(), "gpt-5.6".into()],
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

    // ---- grok (curated: grok-4.5 only) ----
    let pin_owned = grok_pin == "grok-4.5";
    entries.push(ModelEntry {
        id: Cow::Borrowed("grok-4.5"),
        aliases: if pin_owned {
            vec!["grok".to_string()]
        } else {
            Vec::new()
        },
        name: Cow::Borrowed("Grok 4.5"),
        efforts: grok_efforts("grok-4.5"),
        max_context: Some(500_000),
        group: "grok",
    });

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
    fn catalog_matches_user_contract_11_entries() {
        // The pinned (curated) case: exactly 11 rows, claude ids in order.
        let entries = catalog("grok-4.5", "gpt-5.6-sol");
        assert_eq!(entries.len(), 11);
        let claude_ids: Vec<&str> = entries
            .iter()
            .filter(|e| e.group == "claude")
            .map(|e| e.id.as_ref())
            .collect();
        assert_eq!(
            claude_ids,
            vec![
                "claude-fable-5[1m]",
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
        let entries = catalog("grok-4.5", "gpt-5.6-sol");
        let groups: Vec<&str> = entries.iter().map(|e| e.group).collect();
        let first_codex = groups.iter().position(|g| *g == "codex").unwrap();
        let first_grok = groups.iter().position(|g| *g == "grok").unwrap();
        assert!(groups[..first_codex].iter().all(|g| *g == "claude"));
        assert!(groups[first_codex..first_grok]
            .iter()
            .all(|g| *g == "codex"));
        assert!(groups[first_grok..].iter().all(|g| *g == "grok"));
    }

    #[test]
    fn claude_entries_carry_curated_efforts_and_context() {
        let entries = catalog("grok-4.5", "gpt-5.6-sol");
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
        assert_eq!(find(&entries, "claude-opus-4-8[1m]").aliases, vec!["opus"]);
        assert!(find(&entries, "claude-opus-4-6[1m]").aliases.is_empty());
        assert_eq!(
            find(&entries, "claude-sonnet-5[1m]").aliases,
            vec!["sonnet"]
        );
        assert!(find(&entries, "claude-sonnet-5").aliases.is_empty());
        assert_eq!(find(&entries, "claude-haiku-4-5").aliases, vec!["haiku"]);
        assert_eq!(
            find(&entries, "claude-sonnet-5[1m]").max_context,
            Some(1_000_000)
        );
        assert_eq!(find(&entries, "claude-sonnet-5").max_context, Some(200_000));
    }

    #[test]
    fn grok_4_5_context_and_efforts() {
        let entries = catalog("grok-4.5", "gpt-5.6-sol");
        let g = find(&entries, "grok-4.5");
        assert_eq!(g.max_context, Some(500_000));
        assert_eq!(g.efforts, &["low", "medium", "high"]);
    }

    #[test]
    fn grok_family_alias_follows_the_pin() {
        // Curated pin owns the alias directly.
        let pinned = catalog("grok-4.5", "gpt-5.6-sol");
        assert_eq!(find(&pinned, "grok-4.5").aliases, vec!["grok".to_string()]);

        // Out-of-catalog pin: alias moves to the synthesized row.
        let pinned = catalog("grok-4.3", "gpt-5.6-sol");
        assert!(find(&pinned, "grok-4.5").aliases.is_empty());
        assert_eq!(find(&pinned, "grok-4.3").aliases, vec!["grok".to_string()]);
    }

    #[test]
    fn in_catalog_pin_does_not_synthesize_a_row() {
        // A curated pin: no synthesized row, alias on the static row, count 11.
        let entries = catalog("grok-4.5", "gpt-5.6-sol");
        assert_eq!(entries.len(), 11);
        let owners: Vec<&str> = entries
            .iter()
            .filter(|e| e.aliases.iter().any(|a| a == "grok"))
            .map(|e| e.id.as_ref())
            .collect();
        assert_eq!(owners, vec!["grok-4.5"]);
    }

    #[test]
    fn out_of_catalog_pin_synthesizes_a_null_metadata_row() {
        // A pin outside the curated set (routable via provider passthrough)
        // gets exactly one synthesized owner of the "grok" alias.
        let entries = catalog("grok-code-fast-1", "gpt-5.6-sol");
        assert_eq!(entries.len(), 12);
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
        // Appended after the curated grok row (last entry).
        assert_eq!(entries.last().unwrap().id, "grok-code-fast-1");
    }

    #[test]
    fn synthesized_pin_keeps_known_thinking_levels() {
        // A known reasoner pinned outside the curated set still gets its effort
        // menu from the thinking-level lookup, even though metadata is null.
        let entries = catalog("grok-4.3", "gpt-5.6-sol");
        assert_eq!(entries.len(), 12);
        let synth = find(&entries, "grok-4.3");
        assert_eq!(synth.efforts, &["none", "low", "medium", "high"]);
        assert_eq!(synth.max_context, None);
        assert_eq!(synth.aliases, vec!["grok".to_string()]);
    }

    #[test]
    fn gpt_5_6_sol_aliases_context_and_effort_count() {
        let entries = catalog("grok-4.5", "gpt-5.6-sol");
        let sol = find(&entries, "gpt-5.6-sol");
        assert_eq!(sol.aliases, vec!["sol".to_string(), "gpt-5.6".to_string()]);
        assert_eq!(sol.max_context, Some(372_000));
        assert_eq!(sol.efforts.len(), 6);
    }

    #[test]
    fn dropped_ids_are_absent() {
        let entries = catalog("grok-4.5", "gpt-5.6-sol");
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
        let entries = catalog("grok-4.5", "gpt-5.6-sol");
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
    }
}
