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
//! - Codex context windows and effort menus: the openai/codex model catalog
//!   (`models-manager/models.json`), fetched 2026-07-14. `gpt-5.6-sol/terra`
//!   support low..ultra; `gpt-5.6-luna` low..max; `gpt-5.5` low..xhigh. The
//!   `gpt-5.5-codex` / `gpt-5-codex` ids are absent from the current
//!   models.json (legacy passthrough) — context unknown (null), effort menu
//!   the documented low..xhigh floor.
//! - Grok context windows / names: the live `cli-chat-proxy` `/v1/models`
//!   probe 2026-07-14 (`grok-4.5` ctx 500000; `grok-composer-2.5-fast` ctx
//!   200000). `grok-build-0.1` ctx 256000 from `docs/grok/spec.md`. Grok
//!   effort menus come from [`crate::provider::grok::thinking_levels_catalog`]
//!   (models with no thinking support get an empty menu). `grok-4.3` /
//!   `grok-3-mini` context windows are not published → null.
//! - Claude context windows: the Anthropic subscription standard 200000 for
//!   each id. `claude-fable-5` is nominal/unverified (flagged in
//!   `docs/models.md`). llmux does not shape claude requests, so their effort
//!   menus are empty and they carry no aliases (bare names like `opus` ROUTE
//!   but are never resolved to a concrete id).

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

// ---- codex effort menus (openai/codex models.json, 2026-07-14) ----
const CODEX_EFFORTS_SOL_TERRA: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];
const CODEX_EFFORTS_LUNA: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const CODEX_EFFORTS_GPT55: &[&str] = &["low", "medium", "high", "xhigh"];

const CLAUDE_CONTEXT: u64 = 200_000;

/// The known model catalog in canonical group order (`claude < codex < grok`).
/// `grok_pin` / `codex_pin` are the live provider model slugs; the entry whose
/// id equals `grok_pin` additionally advertises the `"grok"` family alias.
///
/// When `grok_pin` matches no curated grok id (a config can pin ANY `grok-*`
/// slug, e.g. `grok-code-fast-1`), a synthesized row is appended after the
/// curated grok entries so the `"grok"` alias always has an owner: id = the
/// pin, name = the pin verbatim, efforts from the thinking-level lookup (else
/// empty), context null. `codex_pin` is unused for aliasing (a model-less
/// codex request uses the pin directly, which is not an alias) — accepted for
/// symmetry / future use.
pub fn catalog(grok_pin: &str, _codex_pin: &str) -> Vec<ModelEntry> {
    let mut entries = Vec::new();

    // ---- claude (pass-through; no aliases, no shaped efforts) ----
    for (id, name) in [
        ("claude-fable-5", "Claude Fable 5"),
        ("claude-opus-4-8", "Claude Opus 4.8"),
        ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
        ("claude-sonnet-4-5", "Claude Sonnet 4.5"),
        ("claude-haiku-4-5", "Claude Haiku 4.5"),
    ] {
        entries.push(ModelEntry {
            id: Cow::Borrowed(id),
            aliases: Vec::new(),
            name: Cow::Borrowed(name),
            efforts: &[],
            max_context: Some(CLAUDE_CONTEXT),
            group: "claude",
        });
    }

    // ---- codex ----
    entries.push(codex_entry(
        "gpt-5.5",
        "GPT-5.5",
        CODEX_EFFORTS_GPT55,
        Some(272_000),
        Vec::new(),
    ));
    entries.push(codex_entry(
        "gpt-5.5-codex",
        "GPT-5.5-Codex",
        CODEX_EFFORTS_GPT55,
        None,
        Vec::new(),
    ));
    entries.push(codex_entry(
        "gpt-5-codex",
        "GPT-5-Codex",
        CODEX_EFFORTS_GPT55,
        None,
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

    // ---- grok (curated) ----
    let mut pin_owned = false;
    for (id, name, ctx) in [
        ("grok-4.5", "Grok 4.5", Some(500_000)),
        ("grok-4.3", "Grok 4.3", None),
        ("grok-3-mini", "Grok 3 Mini", None),
        ("grok-composer-2.5-fast", "Composer 2.5", Some(200_000)),
        ("grok-build-0.1", "Grok Build 0.1", Some(256_000)),
    ] {
        let mut aliases = Vec::new();
        if id == grok_pin {
            aliases.push("grok".to_string());
            pin_owned = true;
        }
        entries.push(ModelEntry {
            id: Cow::Borrowed(id),
            aliases,
            name: Cow::Borrowed(name),
            efforts: grok_efforts(id),
            max_context: ctx,
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
    fn catalog_has_all_groups_in_canonical_order() {
        let entries = catalog("grok-4.5", "gpt-5.6-sol");
        assert_eq!(entries.len(), 5 + 6 + 5);
        let groups: Vec<&str> = entries.iter().map(|e| e.group).collect();
        // claude block, then codex block, then grok block.
        let first_codex = groups.iter().position(|g| *g == "codex").unwrap();
        let first_grok = groups.iter().position(|g| *g == "grok").unwrap();
        assert!(groups[..first_codex].iter().all(|g| *g == "claude"));
        assert!(groups[first_codex..first_grok]
            .iter()
            .all(|g| *g == "codex"));
        assert!(groups[first_grok..].iter().all(|g| *g == "grok"));
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
        let pinned = catalog("grok-4.5", "gpt-5.6-sol");
        assert_eq!(find(&pinned, "grok-4.5").aliases, vec!["grok".to_string()]);
        assert!(find(&pinned, "grok-4.3").aliases.is_empty());

        let pinned = catalog("grok-4.3", "gpt-5.6-sol");
        assert_eq!(find(&pinned, "grok-4.3").aliases, vec!["grok".to_string()]);
        assert!(find(&pinned, "grok-4.5").aliases.is_empty());
    }

    #[test]
    fn in_catalog_pin_does_not_synthesize_a_row() {
        // A curated pin: no synthesized row, alias on the static row, count 16.
        let entries = catalog("grok-4.5", "gpt-5.6-sol");
        assert_eq!(entries.len(), 16);
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
        assert_eq!(entries.len(), 17);
        let owners: Vec<&ModelEntry> = entries
            .iter()
            .filter(|e| e.aliases.iter().any(|a| a == "grok"))
            .collect();
        assert_eq!(owners.len(), 1, "exactly one owner of the grok alias");
        let synth = owners[0];
        assert_eq!(synth.id, "grok-code-fast-1");
        assert_eq!(synth.name, "grok-code-fast-1");
        assert_eq!(synth.max_context, None);
        assert_eq!(synth.group, "grok");
        // Appended after the curated grok rows (last entry).
        assert_eq!(entries.last().unwrap().id, "grok-code-fast-1");
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
    fn claude_entries_have_no_efforts_or_aliases() {
        let entries = catalog("grok-4.5", "gpt-5.6-sol");
        for e in entries.iter().filter(|e| e.group == "claude") {
            assert!(e.efforts.is_empty(), "{}: no shaped efforts", e.id);
            assert!(e.aliases.is_empty(), "{}: no aliases", e.id);
            assert_eq!(e.max_context, Some(200_000));
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
