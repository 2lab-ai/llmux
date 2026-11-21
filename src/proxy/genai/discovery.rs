//! `GET /v1beta/models` — the llmux catalog as the Gemini API model list agy
//! discovers in gateway mode (`gateway.vertexModelEntry`: `name`,
//! `displayName`, `description`, `inputTokenLimit`, `outputTokenLimit`,
//! `supportedGenerationMethods`, `thinking`).
//!
//! What agy does with it (live capture 2026-11-21, agy 1.5.13):
//! `outputTokenLimit` becomes every request's `maxOutputTokens`; `--effort`
//! is refused unless `thinking` is true; with no `--model` agy picks the FIRST
//! entry; ids it does not know take one of 32 placeholder slots and the rest
//! are dropped. Hence the list's rules:
//!
//! 1. One entry per model: a `[1m]` twin and its base row are ONE entry under
//!    the base id (brackets never reach a URL path); a `[1m]`-only row is
//!    listed under its base id too and served as the `[1m]` row
//!    ([`super::served_model`]). The base row's metadata wins when both exist.
//! 2. At most [`MAX_MODELS`] entries. Over the cap the default model is
//!    kept first, then every model a catalog alias points at (each family's
//!    current pick: `opus`, `haiku`, `sol`, `grok`, `argon`, …), then the
//!    rest; within each of those tiers the groups take turns in catalog group
//!    order (each group's rows in catalog order), so no group crowds another
//!    out.
//! 3. The default model for agy comes first; the rest keep catalog order.

use http::StatusCode;
use serde_json::{json, Value};

use crate::catalog::ModelEntry;

/// The most entries the list carries: agy keeps at most 32 ids it does not
/// know (`buildResponseFromAdminModelList`, 32 placeholder slots).
pub(crate) const MAX_MODELS: usize = 32;

/// Gemini API default page size for `models.list`.
const DEFAULT_PAGE_SIZE: usize = 50;
/// Gemini API maximum page size for `models.list`.
const MAX_PAGE_SIZE: usize = 1000;

/// `outputTokenLimit` per backend group (the catalog carries no output caps).
/// agy sends it back as `maxOutputTokens` on every request.
///
/// - claude 64,000: within every current Claude model's output cap; the Agent
///   SDK takes it as `CLAUDE_CODE_MAX_OUTPUT_TOKENS`.
/// - codex 128,000: GPT models' output cap; the codex backend takes no cap
///   anyway (the translation omits it).
/// - grok 32,768 and openrouter 16,384: conservative, forwarded upstream.
/// - gemini 65,536: Gemini 3.x / 4 output cap, forwarded in the native body.
pub(crate) fn output_limit(group: &str) -> u64 {
    match group {
        "claude" => 64_000,
        "codex" => 128_000,
        "gemini" => 65_536,
        "openrouter" => 16_384,
        _ => 32_768,
    }
}

/// One discovery entry before rendering.
#[derive(Debug, Clone)]
struct Entry<'a> {
    id: String,
    row: &'a ModelEntry,
    /// A catalog alias points at this model (its row or its `[1m]` twin).
    aliased: bool,
}

/// The agy-facing id of a catalog row: the id without its `[1m]` suffix.
fn base_id(id: &str) -> &str {
    id.strip_suffix("[1m]").unwrap_or(id)
}

/// The discovery entry id `default_model` names: a row id (with or without
/// `[1m]`) or one of a row's aliases.
pub(crate) fn default_id(rows: &[ModelEntry], default_model: &str) -> Option<String> {
    let wanted = default_model.trim().to_ascii_lowercase();
    let wanted = base_id(&wanted);
    rows.iter()
        .find(|row| base_id(&row.id) == wanted)
        .or_else(|| {
            rows.iter()
                .find(|row| row.aliases.iter().any(|alias| alias == wanted))
        })
        .map(|row| base_id(&row.id).to_string())
}

/// The discovery list for `rows` (catalog order), `default_model` first.
pub(crate) fn models(rows: &[ModelEntry], default_model: &str) -> Vec<Value> {
    let mut entries: Vec<Entry> = Vec::new();
    for row in rows {
        let id = base_id(&row.id);
        let aliased = !row.aliases.is_empty();
        match entries.iter_mut().find(|entry| entry.id == id) {
            Some(entry) => {
                entry.aliased |= aliased;
                // The base row's own metadata (its window) wins over its twin's.
                if row.id == id {
                    entry.row = row;
                }
            }
            None => entries.push(Entry {
                id: id.to_string(),
                row,
                aliased,
            }),
        }
    }
    let pinned = default_id(rows, default_model);
    let keep = capped(&entries, pinned.as_deref());
    let mut out: Vec<&Entry> = entries
        .iter()
        .enumerate()
        .filter(|(index, _)| keep[*index])
        .map(|(_, entry)| entry)
        .collect();
    if let Some(at) = pinned
        .as_deref()
        .and_then(|pinned| out.iter().position(|entry| entry.id == pinned))
    {
        let first = out.remove(at);
        out.insert(0, first);
    }
    out.into_iter().map(render).collect()
}

/// Which entries survive the [`MAX_MODELS`] cap: all when under it; else the
/// pinned one, then the aliased ones, then the rest — each tier filled by
/// [`take_turns`].
fn capped(entries: &[Entry], pinned: Option<&str>) -> Vec<bool> {
    if entries.len() <= MAX_MODELS {
        return vec![true; entries.len()];
    }
    let mut keep = vec![false; entries.len()];
    if let Some(index) = pinned.and_then(|id| entries.iter().position(|entry| entry.id == id)) {
        keep[index] = true;
    }
    for aliased in [true, false] {
        take_turns(entries, &mut keep, |entry| entry.aliased == aliased);
    }
    keep
}

/// Keep the not-yet-kept entries `in_tier` admits, the groups taking turns
/// (group order of first appearance, catalog order within a group), until
/// [`MAX_MODELS`] are kept.
fn take_turns(entries: &[Entry], keep: &mut [bool], in_tier: impl Fn(&Entry) -> bool) {
    let mut kept = keep.iter().filter(|kept| **kept).count();
    let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if keep[index] || !in_tier(entry) {
            continue;
        }
        match groups
            .iter_mut()
            .find(|(group, _)| *group == entry.row.group)
        {
            Some((_, members)) => members.push(index),
            None => groups.push((entry.row.group, vec![index])),
        }
    }
    let mut round = 0;
    while kept < MAX_MODELS {
        let mut took = false;
        for (_, members) in &groups {
            if kept == MAX_MODELS {
                break;
            }
            if let Some(&index) = members.get(round) {
                keep[index] = true;
                kept += 1;
                took = true;
            }
        }
        if !took {
            break;
        }
        round += 1;
    }
}

/// Gemini level suffixes agy recognizes on its own Gemini families.
const GEMINI_LEVELS: &[&str] = &["-minimal", "-low", "-medium", "-high", "-xhigh"];

/// The entry's `thinking` flag: the row has an effort menu — or it is a
/// gemini id whose level rides in its name (`gemini-3.8-flash-low`). agy
/// (1.5.13, live probe 2026-11-21) strips such a suffix from the request URL
/// (`…/models/gemini-3.8-flash:…`) and carries the level as
/// `thinkingLevel: LOW` only when the entry says `thinking: true`; with
/// `false` it sends `thinkingBudget: 0` and the level is lost (the bare family
/// then resolves to `-high`). agy still refuses `--effort` on a
/// level-suffixed id, so this opens no effort menu.
fn thinking(id: &str, row: &ModelEntry) -> bool {
    !row.efforts.is_empty()
        || (row.group == "gemini" && GEMINI_LEVELS.iter().any(|level| id.ends_with(level)))
}

/// One `vertexModelEntry`.
fn render(entry: &Entry) -> Value {
    let row = entry.row;
    let name = row.name.strip_suffix(" [1M]").unwrap_or(&row.name);
    let mut description = format!("llmux · {}", row.group);
    if let (Some(first), Some(last)) = (row.efforts.first(), row.efforts.last()) {
        description.push_str(&format!(" · efforts {first}…{last}"));
    }
    let mut value = json!({
        "name": format!("models/{}", entry.id),
        "baseModelId": entry.id,
        "version": "llmux",
        "displayName": name,
        "description": description,
        "outputTokenLimit": output_limit(row.group),
        "supportedGenerationMethods": ["generateContent", "streamGenerateContent", "countTokens"],
        "thinking": thinking(&entry.id, row),
    });
    if let Some(context) = row.max_context {
        value["inputTokenLimit"] = json!(context);
    }
    value
}

/// One `models.list` page: `pageSize` (default 50, at most 1000) entries from
/// the offset `pageToken` names, with `nextPageToken` while more remain.
/// `Err` is a 400 detail for a token this list never issued.
pub(crate) fn page(
    models: &[Value],
    page_size: Option<&str>,
    page_token: Option<&str>,
) -> Result<Value, (StatusCode, String)> {
    let size = page_size
        .and_then(|size| size.parse::<usize>().ok())
        .filter(|size| *size > 0)
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .min(MAX_PAGE_SIZE);
    let start = match page_token.filter(|token| !token.is_empty()) {
        None => 0,
        Some(token) => token
            .parse::<usize>()
            .ok()
            .filter(|start| *start <= models.len())
            .ok_or((StatusCode::BAD_REQUEST, "invalid pageToken".to_string()))?,
    };
    let end = (start + size).min(models.len());
    let mut page = json!({"models": models[start..end]});
    if end < models.len() {
        page["nextPageToken"] = json!(end.to_string());
    }
    Ok(page)
}

/// `pageSize` / `pageToken` from a query string.
pub(crate) fn page_params(query: Option<&str>) -> (Option<String>, Option<String>) {
    let mut size = None;
    let mut token = None;
    for pair in query.unwrap_or("").split('&') {
        match pair.split_once('=') {
            Some(("pageSize", value)) => size = Some(value.to_string()),
            Some(("pageToken", value)) => token = Some(value.to_string()),
            _ => {}
        }
    }
    (size, token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(models: &[Value]) -> Vec<String> {
        models
            .iter()
            .map(|m| {
                m["name"]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("models/")
                    .to_string()
            })
            .collect()
    }

    fn live_catalog() -> Vec<ModelEntry> {
        crate::catalog::catalog(
            "grok-4.7",
            crate::provider::codex::CODEX_MODEL,
            crate::catalog::OPENROUTER_DEFAULT_PIN,
        )
    }

    #[test]
    fn the_live_catalog_lists_one_entry_per_model_default_first() {
        let rows = live_catalog();
        let models = models(&rows, "gpt-5.6-sol");
        let ids = ids(&models);
        assert!(ids.len() <= MAX_MODELS, "{} entries", ids.len());
        assert_eq!(ids[0], "gpt-5.6-sol", "the default comes first");
        assert!(ids.iter().all(|id| !id.contains('[')), "{ids:?}");
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "no duplicates: {ids:?}");
        // Catalog order after the default.
        let position = |id: &str| ids.iter().position(|x| x == id).unwrap();
        assert!(position("claude-fable-5-2") < position("claude-opus-5-5"));
        assert!(position("claude-opus-5-5") < position("gpt-6.2-sol"));
        assert!(position("gpt-6.2-sol") < position("grok-4.7"));
        assert!(position("grok-4.7") < position("gemini-3.8-flash-high"));
        // Over the cap, every family's current pick (an alias owner) stays.
        for current in [
            "claude-fable-5-2",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "gpt-6.2-sol",
            "gpt-6.2-astra",
            "gpt-6.1-terra",
            "gpt-6-luna",
            "grok-4.7",
            "or-nemotron-3-ultra",
            "gemini-3.8-flash-high",
            "gemini-4-argon",
        ] {
            assert!(ids.iter().any(|id| id == current), "{current}: {ids:?}");
        }
        // Every group is represented.
        for group in ["claude", "codex", "grok", "openrouter", "gemini"] {
            assert!(
                models
                    .iter()
                    .any(|m| m["description"].as_str().unwrap().contains(group)),
                "{group}"
            );
        }
    }

    #[test]
    fn entries_carry_the_fields_agy_reads() {
        let rows = live_catalog();
        let models = models(&rows, "claude-opus-5-5");
        let opus = &models[0];
        assert_eq!(opus["name"], "models/claude-opus-5-5");
        assert_eq!(opus["displayName"], "Claude Opus 5.5");
        assert_eq!(opus["inputTokenLimit"], 200_000, "the base row's window");
        assert_eq!(opus["outputTokenLimit"], 64_000);
        assert_eq!(opus["thinking"], true, "claude rows have an effort menu");
        assert_eq!(
            opus["supportedGenerationMethods"],
            json!(["generateContent", "streamGenerateContent", "countTokens"])
        );
        let find = |id: &str| {
            models
                .iter()
                .find(|m| m["name"] == format!("models/{id}"))
                .unwrap_or_else(|| panic!("{id}"))
                .clone()
        };
        // A `[1m]`-only row is listed under its base id with its own window.
        let opus48 = find("claude-opus-4-8");
        assert_eq!(opus48["inputTokenLimit"], 1_000_000);
        // A gemini id that names its level says `thinking`, so agy carries
        // the level it strips from the URL; a Gemini 4 family row has an
        // effort menu (from the provider's family table), so it says it too.
        let flash = find("gemini-3.8-flash-low");
        assert_eq!(flash["thinking"], true);
        assert_eq!(flash["outputTokenLimit"], 65_536);
        for family in [
            "gemini-4-argon",
            "gemini-4-neon",
            "gemini-4-carbon",
            "gemini-4-hydrogen",
        ] {
            assert_eq!(find(family)["thinking"], true, "{family}");
        }
        // A row with neither an effort menu nor a level in its name does not.
        assert_eq!(find("or-free")["thinking"], false);
        assert_eq!(find("gpt-6.2-sol")["outputTokenLimit"], 128_000);
    }

    #[test]
    fn a_default_alias_resolves_to_its_row() {
        let rows = live_catalog();
        assert_eq!(ids(&models(&rows, "opus"))[0], "claude-opus-5-5");
        assert_eq!(ids(&models(&rows, "argon"))[0], "gemini-4-argon");
        assert_eq!(ids(&models(&rows, "gpt-6.2-sol[1m]"))[0], "gpt-6.2-sol");
        // An unknown default pins nothing: plain catalog order.
        assert_eq!(ids(&models(&rows, "nope"))[0], "claude-fable-5-2");
    }

    /// Over the cap, groups take turns: the big groups lose their tail, the
    /// small ones keep everything, and the default survives wherever it is.
    #[test]
    fn the_cap_shares_slots_between_groups_and_keeps_the_default() {
        let leaked: &'static [&'static str] = &["low", "high"];
        let rows: Vec<ModelEntry> = (0..30)
            .map(|i| ("claude", format!("claude-x-{i}")))
            .chain((0..30).map(|i| ("codex", format!("gpt-x-{i}"))))
            .chain((0..3).map(|i| ("gemini", format!("gemini-x-{i}"))))
            .map(|(group, id)| ModelEntry {
                id: id.clone().into(),
                aliases: Vec::new(),
                name: id.into(),
                efforts: leaked,
                max_context: Some(1),
                group,
            })
            .collect();
        let ids = ids(&models(&rows, "gpt-x-29"));
        assert_eq!(ids.len(), MAX_MODELS);
        assert_eq!(ids[0], "gpt-x-29");
        for i in 0..3 {
            assert!(ids.contains(&format!("gemini-x-{i}")), "{ids:?}");
        }
        let claude = ids.iter().filter(|id| id.starts_with("claude")).count();
        let codex = ids.iter().filter(|id| id.starts_with("gpt")).count();
        assert_eq!(claude + codex, MAX_MODELS - 3);
        assert!(claude.abs_diff(codex) <= 2, "{claude} vs {codex}");
        assert!(ids.contains(&"claude-x-0".to_string()));
        assert!(!ids.contains(&"claude-x-29".to_string()));
    }

    #[test]
    fn pages_follow_the_token() {
        let models: Vec<Value> = (0..5)
            .map(|i| json!({"name": format!("models/m{i}")}))
            .collect();
        let first = page(&models, Some("2"), None).unwrap();
        assert_eq!(first["models"].as_array().unwrap().len(), 2);
        assert_eq!(first["nextPageToken"], "2");
        let last = page(&models, Some("2"), Some("4")).unwrap();
        assert_eq!(last["models"].as_array().unwrap().len(), 1);
        assert!(last.get("nextPageToken").is_none());
        let all = page(&models, Some("1000"), None).unwrap();
        assert_eq!(all["models"].as_array().unwrap().len(), 5);
        assert!(all.get("nextPageToken").is_none());
        assert_eq!(
            page(&models, None, Some("x")).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            page_params(Some("pageSize=1000&pageToken=4&alt=json")),
            (Some("1000".to_string()), Some("4".to_string()))
        );
    }
}
