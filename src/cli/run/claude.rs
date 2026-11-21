//! The Claude Code frontend of `llmux run`: spawn `claude` with the proxy env
//! injected and (unless opted out) the llmux model catalog injected into
//! Claude Code: the `/model` picker lineup as a `--settings` document, and the
//! catalog's alias owners as `ANTHROPIC_DEFAULT_*_MODEL` exports (the only way
//! the aliases Claude Code resolves NATIVELY — `opus`, `fable`, `sonnet`,
//! `haiku` — can mean what the llmux catalog says they mean).

use std::convert::Infallible;

use serde::Serialize;

use super::{fetch_catalog, row_description, spawn, CatalogRow, Frontend};
use crate::cli::{CliError, Endpoint};

/// One `modelPicker` row of the Claude Code `--settings` document. Field order
/// is the serialized key order (`model, label, description`) — `serde` writes
/// struct fields in declaration order, which keeps the emitted JSON stable
/// enough to assert on.
#[derive(Debug, Serialize)]
struct PickerOption {
    /// Taken verbatim by Claude Code — the string llmux routes on.
    model: String,
    label: String,
    description: String,
}

#[derive(Debug, Serialize)]
struct PickerLineup {
    options: Vec<PickerOption>,
}

/// The whole `--settings` document. `replaceBuiltInOptions` is deliberately NOT
/// emitted: the built-in Anthropic rows stay, and Claude Code drops a listed
/// model the built-in lineup already covers.
#[derive(Debug, Serialize)]
struct PickerSettings {
    #[serde(rename = "modelPicker")]
    model_picker: PickerLineup,
}

/// Build the `claude --settings` JSON that lists the llmux catalog in the
/// `/model` picker, PURELY from the fetched rows (so the whole shape is
/// unit-testable without a daemon or a child process):
///
/// - `model` = the catalog `id` verbatim (`[1m]` suffixes included — the
///   provider strips them upstream).
/// - `label` = the catalog `name`.
/// - `description` = `"<group> · efforts <first…last> · ctx <max_context>"`,
///   dropping the efforts part when the menu is empty and the ctx part when the
///   window is unpublished. A single-entry menu renders as that one value.
///
/// Gemini rows (group `gemini`) are left out: Google's Antigravity backend
/// refuses Claude Code requests, so gemini is Codex-only (`run --codex`).
///
/// Catalog order is preserved. `None` when no row remains (an empty or
/// gemini-only catalog) — there is no lineup to inject, and an empty
/// `options` array would only add noise.
fn model_picker_settings(models: &[CatalogRow]) -> Option<String> {
    let options: Vec<PickerOption> = models
        .iter()
        .filter(|row| row.group != "gemini")
        .map(|row| PickerOption {
            model: row.id.clone(),
            label: row.name.clone(),
            description: row_description(row),
        })
        .collect();
    if options.is_empty() {
        return None;
    }
    let settings = PickerSettings {
        model_picker: PickerLineup { options },
    };
    // A document of owned strings cannot fail to serialize.
    serde_json::to_string(&settings).ok()
}

/// The Claude Code aliases that never reach llmux, paired with the env var
/// that redirects each one. Measured 2026-09-28 with Claude Code 2.1.283:
/// `sonnet` / `opus` / `haiku` / `fable` (and their `[1m]` spellings) are
/// NATIVE picker entries the client resolves against its OWN model records
/// before a request is built, so `/model opus` sends `claude-opus-5-5` and the
/// client sizes the session at 200k — llmux's catalog promise that the alias
/// `opus` means `claude-opus-5-5[1m]` never gets a chance to apply. Exporting
/// `ANTHROPIC_DEFAULT_OPUS_MODEL=claude-opus-5-5[1m]` re-points the native
/// record at the catalog's own id (measured: status line "Claude Opus 5.5
/// [1M]", `/context` 221.2k/800k, versus 200k without it).
///
/// Only the `_MODEL` variants are set. Claude Code also reads `_NAME`,
/// `_DESCRIPTION` and `_SUPPORTED_CAPABILITIES` for these families; llmux
/// leaves them alone so the client keeps its own labels.
///
/// These four vars are the ONLY mechanism that changes what a bare NATIVE
/// alias means. They do not generalize: for an id Claude Code does not know
/// (`astra`, `grok`, `or-…`) there is no such var, the id is submitted
/// verbatim, and the client applies its 200k assumption unless the SUBMITTED
/// id ends in `[1m]` — catalog alias ownership decides the upstream slug, not
/// the client's window.
const ALIAS_ENV: &[(&str, &str)] = &[
    ("opus", "ANTHROPIC_DEFAULT_OPUS_MODEL"),
    ("fable", "ANTHROPIC_DEFAULT_FABLE_MODEL"),
    ("sonnet", "ANTHROPIC_DEFAULT_SONNET_MODEL"),
    ("haiku", "ANTHROPIC_DEFAULT_HAIKU_MODEL"),
];

/// The `ANTHROPIC_DEFAULT_*_MODEL` exports for this catalog, PURELY from the
/// fetched rows (so the mapping is unit-testable without a daemon or a child
/// process): for each pair in [`ALIAS_ENV`], the FIRST row that advertises the
/// alias contributes `(var, row.id)`.
///
/// Two rows are deliberately dropped:
/// - an alias no catalog row owns exports nothing (a family llmux does not
///   curate keeps Claude Code's own default);
/// - a var `already_set` in the environment is left alone — the user's own
///   export outranks the catalog, exactly as their `--settings` outranks the
///   picker lineup. The caller passes `|var| std::env::var_os(var).is_some()`;
///   the predicate is a parameter so tests need no process-global mutation.
pub(super) fn alias_env(
    models: &[CatalogRow],
    already_set: impl Fn(&str) -> bool,
) -> Vec<(&'static str, String)> {
    ALIAS_ENV
        .iter()
        .filter(|(_, var)| !already_set(var))
        .filter_map(|&(alias, var)| {
            models
                .iter()
                .find(|row| row.aliases.iter().any(|a| a == alias))
                .map(|row| (var, row.id.clone()))
        })
        .collect()
}

/// Does the user's pass-through arg list already carry `--settings`? Claude
/// Code takes ONE settings document, and the user's lineup wins — llmux never
/// merges into it.
///
/// `--settings` is long-only in Claude Code 2.1.274 (`--settings
/// <file-or-json>`; no short alias), so only the two spellings the flag has are
/// matched: the bare token and `--settings=<value>`.
fn has_user_settings(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg == "--settings" || arg.starts_with("--settings="))
}

/// Should `run` inject the picker lineup? `args` is the pass-through list with
/// the leading `--` ALREADY STRIPPED (as `run` does before spawning), so a
/// user's `--settings` is visible here as its own token.
fn injects_model_picker(args: &[String], no_model_picker: bool) -> bool {
    !no_model_picker && !has_user_settings(args)
}

/// Everything ONE catalog fetch feeds into the `claude` launch: the
/// `--settings <json>` argv pair to PREPEND to the user's pass-through args
/// (empty when no lineup is injected), and the `ANTHROPIC_DEFAULT_*_MODEL`
/// exports (empty when nothing is to be exported).
///
/// The two opt-outs are deliberately asymmetric:
/// - `--no-model-picker` skips BOTH (and the fetch) — it is the "leave my
///   Claude Code alone" switch;
/// - the user's own `--settings` skips only the picker DOCUMENT. The alias
///   exports are env vars, not a settings document, so they cannot collide
///   with the user's lineup and still apply.
///
/// Every failure mode is non-fatal: one warning line, then the launch
/// continues unchanged.
async fn catalog_args(
    endpoint: &Endpoint,
    claude_args: &[String],
    no_picker: bool,
) -> (Vec<String>, Vec<(&'static str, String)>) {
    if no_picker {
        return (Vec::new(), Vec::new());
    }
    let models = match fetch_catalog(&endpoint.base_url, endpoint.api_key.as_deref()).await {
        Ok(models) => models,
        Err(reason) => {
            eprintln!("warning: model picker not injected: {reason}");
            return (Vec::new(), Vec::new());
        }
    };
    let env = alias_env(&models, |var| std::env::var_os(var).is_some());
    if !injects_model_picker(claude_args, no_picker) {
        eprintln!("warning: --settings given, llmux model picker lineup not injected");
        return (Vec::new(), env);
    }
    match model_picker_settings(&models) {
        Some(json) => (vec!["--settings".into(), json], env),
        None => {
            eprintln!(
                "warning: model picker not injected: catalog has no Claude Code-selectable models"
            );
            (Vec::new(), env)
        }
    }
}

/// Decide the Claude Code environment for `run`, PURELY from the resolved
/// endpoint (so it is unit-testable without spawning a child): the
/// `ANTHROPIC_BASE_URL` to export, an optional `ANTHROPIC_API_KEY` to set, and
/// whether to REMOVE an inherited `ANTHROPIC_API_KEY`.
///
/// - remote + key    → export the remote's key (overrides any inherited one).
/// - remote + no key → remove `ANTHROPIC_API_KEY` so a parent shell's unrelated
///   upstream key cannot leak to the remote proxy over plain HTTP.
/// - local           → neither set nor remove: Claude Code keeps its own OAuth
///   token (accepted from localhost), which keeps it in subscription mode.
fn claude_env(endpoint: &Endpoint) -> (String, Option<String>, bool) {
    if endpoint.remote {
        match &endpoint.api_key {
            Some(key) => (endpoint.base_url.clone(), Some(key.clone()), false),
            None => (endpoint.base_url.clone(), None, true),
        }
    } else {
        (endpoint.base_url.clone(), None, false)
    }
}

/// Spawn `claude` with `ANTHROPIC_BASE_URL` pointed at `endpoint` and the
/// pass-through args. Local: only `ANTHROPIC_BASE_URL` is set — Claude Code
/// keeps its own OAuth token (which the proxy accepts from localhost); not
/// setting `ANTHROPIC_API_KEY` keeps it in subscription mode. Remote:
/// `ANTHROPIC_API_KEY` is exported with the remote's `x-api-key` (see
/// [`claude_env`]).
///
/// The catalog of the proxy being pointed at is fetched ONCE and feeds two
/// things: it is passed as `claude --settings '<modelPicker lineup>'` so
/// `/model` lists the llmux models (see [`model_picker_settings`]), and its
/// alias owners are exported as `ANTHROPIC_DEFAULT_{OPUS,FABLE,SONNET,HAIKU}_MODEL`
/// so Claude Code's NATIVE aliases resolve to the catalog ids instead of its
/// own (see [`alias_env`]). `--no-model-picker` suppresses both; the user's
/// own `--settings` suppresses only the lineup.
pub(super) async fn launch(
    endpoint: &Endpoint,
    claude_args: &[String],
    no_picker: bool,
) -> Result<Infallible, CliError> {
    // The picker lineup goes FIRST so the user's pass-through args still have
    // the last word on every other flag.
    let (picker, alias_exports) = catalog_args(endpoint, claude_args, no_picker).await;

    let (base_url, api_key, remove_key) = claude_env(endpoint);
    let mut command = tokio::process::Command::new(Frontend::ClaudeCode.binary());
    let launch_model = crate::cli::claude_context::launch_model(claude_args, &alias_exports);
    if launch_model.is_none() {
        tracing::debug!(
            "initial quota/title has no proven launch model; waiting for main-session context"
        );
    }
    let headers = crate::cli::claude_context::custom_headers(
        std::env::var("ANTHROPIC_CUSTOM_HEADERS").ok().as_deref(),
        launch_model.as_deref(),
        crate::proxy::raw_io::now_ms(),
    );
    if headers.is_empty() {
        command.env_remove("ANTHROPIC_CUSTOM_HEADERS");
    } else {
        command.env("ANTHROPIC_CUSTOM_HEADERS", headers);
    }

    command
        .args(&picker)
        .args(claude_args)
        .env("ANTHROPIC_BASE_URL", &base_url);
    for (var, model) in &alias_exports {
        command.env(var, model);
    }
    if let Some(key) = &api_key {
        command.env("ANTHROPIC_API_KEY", key);
    } else if remove_key {
        // Block a parent shell's unrelated upstream ANTHROPIC_API_KEY from
        // being inherited and leaking to the remote proxy over plain HTTP.
        command.env_remove("ANTHROPIC_API_KEY");
    }
    spawn(command, Frontend::ClaudeCode, Vec::new()).await
}

#[cfg(test)]
mod tests {
    use super::super::tests::{row, spawn_models_mock};
    use super::super::CatalogResponse;
    use super::*;

    fn endpoint(remote: bool, api_key: Option<&str>) -> Endpoint {
        Endpoint {
            base_url: "http://llmux-host:3456".into(),
            api_key: api_key.map(Into::into),
            remote,
            host: "llmux-host".into(),
            port: 3456,
        }
    }

    #[test]
    fn claude_env_remote_with_key_exports_it() {
        let (base_url, key, remove) = claude_env(&endpoint(true, Some("lm-remote")));
        assert_eq!(base_url, "http://llmux-host:3456");
        assert_eq!(key.as_deref(), Some("lm-remote"));
        assert!(!remove);
    }

    #[test]
    fn claude_env_remote_without_key_removes_inherited() {
        // The leak guard: no remote key → REMOVE any inherited ANTHROPIC_API_KEY
        // so a parent shell's unrelated upstream key can't hit the remote proxy.
        let (_base_url, key, remove) = claude_env(&endpoint(true, None));
        assert!(key.is_none());
        assert!(remove);
    }

    #[test]
    fn claude_env_local_neither_sets_nor_removes() {
        // Unchanged local behavior: keep Claude Code's own OAuth token.
        let (_base_url, key, remove) = claude_env(&endpoint(false, Some("lm-local")));
        assert!(key.is_none());
        assert!(!remove);
    }

    /// A claude row that owns `aliases` — the shape [`alias_env`] reads.
    fn aliased(id: &str, aliases: &[&str]) -> CatalogRow {
        CatalogRow {
            aliases: aliases.iter().map(|a| (*a).to_string()).collect(),
            ..row(id, id, &[], None, "claude")
        }
    }

    /// Three catalog shapes in one document: a full claude row, a codex row
    /// with NO effort menu, and a grok row with an UNPUBLISHED context window.
    /// Asserted as an exact string — the argv llmux hands `claude` is the
    /// contract, key order included.
    #[test]
    fn model_picker_settings_emits_the_expected_document() {
        let models = [
            row(
                "claude-fable-5-1[1m]",
                "Claude Fable 5.1",
                &["low", "medium", "high", "xhigh", "max"],
                Some(1_000_000),
                "claude",
            ),
            row("gpt-5.5", "GPT-5.5", &[], Some(272_000), "codex"),
            row("grok-4.6", "Grok 4.6", &["low", "high"], None, "grok"),
        ];
        let json = model_picker_settings(&models).expect("non-empty catalog yields a document");
        assert_eq!(
            json,
            concat!(
                r#"{"modelPicker":{"options":["#,
                r#"{"model":"claude-fable-5-1[1m]","label":"Claude Fable 5.1","#,
                r#""description":"claude · efforts low…max · ctx 1000000"},"#,
                r#"{"model":"gpt-5.5","label":"GPT-5.5","description":"codex · ctx 272000"},"#,
                r#"{"model":"grok-4.6","label":"Grok 4.6","description":"grok · efforts low…high"}"#,
                r#"]}}"#,
            )
        );
        // `replaceBuiltInOptions` must stay unset: the built-in rows remain.
        assert!(!json.contains("replaceBuiltInOptions"), "{json}");
    }

    /// A single-value effort menu renders as that one value, not `low…low`.
    #[test]
    fn model_picker_settings_collapses_a_single_effort() {
        let models = [row("or-x", "X", &["high"], None, "openrouter")];
        let json = model_picker_settings(&models).unwrap();
        assert!(
            json.contains(r#""description":"openrouter · efforts high""#),
            "{json}"
        );
    }

    #[test]
    fn model_picker_settings_is_none_for_an_empty_catalog() {
        assert!(model_picker_settings(&[]).is_none());
    }

    /// End-to-end over the REAL catalog module: the document the daemon serves
    /// must parse into [`CatalogRow`] (this is the CLI-side mirror's only
    /// guard against a field rename in `src/catalog.rs`) and yield one picker
    /// row per catalog entry, in catalog order.
    #[test]
    fn model_picker_settings_round_trips_the_real_catalog() {
        let entries = crate::catalog::catalog(
            "grok-4.6",
            "gpt-5.6-sol",
            crate::catalog::OPENROUTER_DEFAULT_PIN,
        );
        let doc = serde_json::json!({ "models": entries }).to_string();
        let rows = serde_json::from_str::<CatalogResponse>(&doc)
            .expect("the served catalog parses as CLI catalog rows")
            .models;
        assert_eq!(rows.len(), entries.len());
        let json = model_picker_settings(&rows).unwrap();
        // Fable 5.2 (2026-11-10) rides the picker exactly as Fable 5.1 does:
        // its single `[1m]` id verbatim, first in catalog order.
        assert!(
            json.contains(r#"{"modelPicker":{"options":[{"model":"claude-fable-5-2[1m]","label":"Claude Fable 5.2","description":"claude · efforts low…max · ctx 1000000"},"#),
            "{json}"
        );
        assert!(
            json.contains(r#"{"model":"claude-fable-5-1[1m]","label":"Claude Fable 5.1","description":"claude · efforts low…max · ctx 1000000"}"#),
            "{json}"
        );
        // The gpt-6.2 rows (2026-11-18) lead the codex block, each id verbatim.
        for option in [
            r#"{"model":"gpt-6.2-sol[1m]","label":"GPT-6.2-Sol [1M]","description":"codex · efforts low…ultra · ctx 1000000"}"#,
            r#"{"model":"gpt-6.2-sol","label":"GPT-6.2-Sol","description":"codex · efforts low…ultra · ctx 500000"}"#,
            r#"{"model":"gpt-6.2-astra[1m]","label":"GPT-6.2-Astra [1M]","description":"codex · efforts low…ultra · ctx 1000000"}"#,
            r#"{"model":"gpt-6.2-astra","label":"GPT-6.2-Astra","description":"codex · efforts low…ultra · ctx 272000"}"#,
            r#"{"model":"gpt-6.1-sol","label":"GPT-6.1-Sol","description":"codex · efforts low…ultra · ctx 500000"}"#,
        ] {
            assert!(json.contains(option), "{option} in {json}");
        }
        // The retired openrouter rows (#243) are gone from the picker.
        assert!(!json.contains("or-ox-alpha"), "{json}");
        let gemini_rows = rows.iter().filter(|row| row.group == "gemini").count();
        assert!(gemini_rows > 0, "the real catalog carries gemini rows");
        assert_eq!(
            json.matches(r#"{"model":"#).count(),
            entries.len() - gemini_rows,
            "one picker row per non-gemini catalog entry"
        );
        assert!(!json.contains("gemini"), "{json}");
    }

    /// Gemini rows never reach Claude Code's picker (Google refuses Claude
    /// Code traffic); every other row keeps its catalog order.
    #[test]
    fn model_picker_settings_drops_gemini_rows() {
        let models = [
            row("claude-opus-5-5", "Claude Opus 5.5", &[], None, "claude"),
            row("gemini-3.8-flash-high[1m]", "Gemini", &[], None, "gemini"),
            row("gpt-5.5", "GPT-5.5", &[], None, "codex"),
            row(
                "gemini-4-argon",
                "Gemini 4 Argon",
                &["low", "medium", "high"],
                Some(1_048_576),
                "gemini",
            ),
            row("grok-4.6", "Grok 4.6", &[], None, "grok"),
        ];
        let json = model_picker_settings(&models).unwrap();
        assert_eq!(
            json,
            concat!(
                r#"{"modelPicker":{"options":["#,
                r#"{"model":"claude-opus-5-5","label":"Claude Opus 5.5","description":"claude"},"#,
                r#"{"model":"gpt-5.5","label":"GPT-5.5","description":"codex"},"#,
                r#"{"model":"grok-4.6","label":"Grok 4.6","description":"grok"}"#,
                r#"]}}"#,
            )
        );
    }

    /// A gemini-only catalog leaves nothing to inject — same as empty.
    #[test]
    fn model_picker_settings_is_none_for_a_gemini_only_catalog() {
        let models = [
            row("gemini-3.8-flash-high[1m]", "Gemini", &[], None, "gemini"),
            row(
                "gemini-4-argon",
                "Gemini 4 Argon",
                &["low", "medium", "high"],
                Some(1_048_576),
                "gemini",
            ),
        ];
        assert!(model_picker_settings(&models).is_none());
    }

    /// A fixture mirroring the real claude block: the `[1m]` twins own the
    /// bare aliases, the plain rows own none, and `haiku` has no twin at all.
    fn claude_rows() -> Vec<CatalogRow> {
        vec![
            aliased("claude-fable-5-2[1m]", &["fable", "fable-5-2"]),
            aliased("claude-fable-5-1[1m]", &["fable-5-1"]),
            aliased("claude-fable-5[1m]", &[]),
            aliased("claude-opus-5-5[1m]", &["opus", "opus-5-5"]),
            aliased("claude-opus-5-5", &[]),
            aliased("claude-sonnet-5-5[1m]", &["sonnet", "sonnet-5-5"]),
            aliased("claude-sonnet-5-5", &[]),
            aliased("claude-sonnet-5[1m]", &["sonnet-5"]),
            aliased("claude-sonnet-5", &[]),
            aliased("claude-haiku-4-5", &["haiku"]),
        ]
    }

    /// The whole point of the export: each Claude Code family var gets the
    /// catalog's OWN id for that alias — the `[1m]` row where one exists, so
    /// `/model opus` stops resolving to the client's 200k `claude-opus-5-5`.
    /// Order follows `ALIAS_ENV`, not catalog order.
    #[test]
    fn alias_env_exports_the_catalog_alias_owners() {
        let env = alias_env(&claude_rows(), |_| false);
        assert_eq!(
            env,
            vec![
                ("ANTHROPIC_DEFAULT_OPUS_MODEL", "claude-opus-5-5[1m]".into()),
                (
                    "ANTHROPIC_DEFAULT_FABLE_MODEL",
                    "claude-fable-5-2[1m]".into()
                ),
                (
                    "ANTHROPIC_DEFAULT_SONNET_MODEL",
                    "claude-sonnet-5-5[1m]".into()
                ),
                ("ANTHROPIC_DEFAULT_HAIKU_MODEL", "claude-haiku-4-5".into()),
            ]
        );
    }

    /// The user's own export wins: a var already in the environment is left
    /// alone, and the other three still go out.
    #[test]
    fn alias_env_leaves_an_already_set_var_alone() {
        let env = alias_env(&claude_rows(), |var| var == "ANTHROPIC_DEFAULT_OPUS_MODEL");
        let vars: Vec<&str> = env.iter().map(|(var, _)| *var).collect();
        assert_eq!(
            vars,
            vec![
                "ANTHROPIC_DEFAULT_FABLE_MODEL",
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            ]
        );
    }

    /// An alias no row owns exports nothing — that family keeps Claude Code's
    /// own default rather than being pointed at a guess. An empty catalog is
    /// the degenerate case of the same rule.
    #[test]
    fn alias_env_skips_an_unowned_alias() {
        let rows = vec![aliased("claude-haiku-4-5", &["haiku"])];
        assert_eq!(
            alias_env(&rows, |_| false),
            vec![(
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                "claude-haiku-4-5".to_string()
            )]
        );
        assert!(alias_env(&[], |_| false).is_empty());
    }

    /// End-to-end over the REAL catalog module, like
    /// [`model_picker_settings_round_trips_the_real_catalog`]: the served
    /// document must parse into [`CatalogRow`] *with its aliases* and yield all
    /// four exports. This is what fails if a future re-curation moves an alias
    /// off a `[1m]` row (or drops one), which would silently restore Claude
    /// Code's 200k denominator.
    #[test]
    fn alias_env_covers_every_family_of_the_real_catalog() {
        let entries = crate::catalog::catalog(
            "grok-4.7",
            "gpt-5.6-sol",
            crate::catalog::OPENROUTER_DEFAULT_PIN,
        );
        let doc = serde_json::json!({ "models": entries }).to_string();
        let rows = serde_json::from_str::<CatalogResponse>(&doc)
            .expect("the served catalog parses as CLI catalog rows")
            .models;
        assert_eq!(
            alias_env(&rows, |_| false),
            vec![
                ("ANTHROPIC_DEFAULT_OPUS_MODEL", "claude-opus-5-5[1m]".into()),
                (
                    "ANTHROPIC_DEFAULT_FABLE_MODEL",
                    "claude-fable-5-2[1m]".into()
                ),
                (
                    "ANTHROPIC_DEFAULT_SONNET_MODEL",
                    "claude-sonnet-5-5[1m]".into()
                ),
                (
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                    "claude-haiku-5-5[1m]".into()
                ),
            ]
        );
    }

    /// The injection decision. `args` here is always the list with a leading
    /// `--` already stripped (what `run` passes), so a user `--settings` is a
    /// token of its own.
    #[test]
    fn injects_model_picker_matrix() {
        let plain: Vec<String> = vec!["--model".into(), "opus".into(), "-p".into()];
        assert!(injects_model_picker(&plain, false), "plain args inject");
        assert!(injects_model_picker(&[], false), "no args inject");

        assert!(
            !injects_model_picker(&plain, true),
            "--no-model-picker opts out"
        );

        let separate: Vec<String> = vec!["--settings".into(), "x.json".into()];
        assert!(
            !injects_model_picker(&separate, false),
            "user --settings wins"
        );
        let inline: Vec<String> = vec!["--settings=x.json".into()];
        assert!(
            !injects_model_picker(&inline, false),
            "user --settings=<v> wins"
        );

        // Not a false positive: a value that merely mentions the flag name.
        let lookalike: Vec<String> = vec!["--model".into(), "settings".into()];
        assert!(injects_model_picker(&lookalike, false), "{lookalike:?}");
    }

    /// Serve a one-row claude catalog and drive [`catalog_args`] end to end.
    /// `--no-model-picker` opts out of BOTH products; the user's own
    /// `--settings` drops only the lineup and keeps the env exports (they are
    /// env vars, not a settings document, so they cannot collide with it).
    #[tokio::test]
    async fn catalog_args_opt_outs_are_asymmetric() {
        let base_url = spawn_models_mock(
            http::StatusCode::OK,
            serde_json::json!({
                "models": [{
                    "id": "claude-opus-5-5[1m]",
                    "aliases": ["opus", "opus-5-5"],
                    "name": "Claude Opus 5.5 [1M]",
                    "efforts": ["low", "max"],
                    "max_context": 1_000_000u64,
                    "group": "claude",
                }]
            })
            .to_string(),
        )
        .await;
        let endpoint = Endpoint {
            base_url: base_url.clone(),
            api_key: None,
            remote: false,
            host: "127.0.0.1".into(),
            port: 0,
        };
        // The env half is asserted against the process environment rather than
        // a fixed list, so a shell that already exports one of the four vars
        // cannot make this flaky — that var is simply left alone.
        let opus_expected = std::env::var_os("ANTHROPIC_DEFAULT_OPUS_MODEL").is_none();

        let (picker, env) = catalog_args(&endpoint, &[], false).await;
        assert_eq!(picker.first().map(String::as_str), Some("--settings"));
        assert!(
            picker[1].contains(r#""model":"claude-opus-5-5[1m]""#),
            "{picker:?}"
        );
        assert_eq!(
            env.iter()
                .any(|(var, _)| *var == "ANTHROPIC_DEFAULT_OPUS_MODEL"),
            opus_expected
        );
        // Only the owned alias is exported: the fixture has no fable/sonnet/
        // haiku row, so those three families keep Claude Code's defaults.
        assert!(env.len() <= 1, "{env:?}");

        let user_settings: Vec<String> = vec!["--settings".into(), "mine.json".into()];
        let (picker, env_with_settings) = catalog_args(&endpoint, &user_settings, false).await;
        assert!(picker.is_empty(), "user --settings wins the lineup");
        assert_eq!(env_with_settings, env, "alias exports still apply");

        let (picker, env) = catalog_args(&endpoint, &[], true).await;
        assert!(picker.is_empty(), "--no-model-picker skips the lineup");
        assert!(env.is_empty(), "--no-model-picker skips the exports too");
    }

    /// `--no-model-picker` must skip the FETCH, not just its products: it is
    /// the "leave my Claude Code alone" switch, so the launch must not touch
    /// the daemon for a catalog it will not use. Observed on the server side
    /// with a request counter — the second call proves the counter is wired,
    /// so a broken mock cannot make the first assertion pass vacuously.
    #[tokio::test]
    async fn catalog_args_with_no_picker_never_fetches_the_catalog() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let app = axum::Router::new().route(
            "/llmux/models",
            axum::routing::get(move || {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    (http::StatusCode::OK, r#"{"models":[]}"#.to_string())
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let endpoint = Endpoint {
            base_url: format!("http://127.0.0.1:{port}"),
            api_key: None,
            remote: false,
            host: "127.0.0.1".into(),
            port,
        };

        let (picker, env) = catalog_args(&endpoint, &[], true).await;
        assert!(picker.is_empty());
        assert!(env.is_empty());
        assert_eq!(hits.load(Ordering::SeqCst), 0, "no catalog fetch was made");

        // Same endpoint, picker enabled: the counter moves, so the zero above
        // is a real observation and not a dead route.
        let _ = catalog_args(&endpoint, &[], false).await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    /// A failed fetch yields NEITHER product — no lineup and no alias exports
    /// (an export built from a catalog llmux could not read would be a guess).
    /// The launch continues; only a warning line is printed.
    #[tokio::test]
    async fn catalog_args_yields_nothing_when_the_fetch_fails() {
        // Bind then drop to reserve-and-free a port nobody listens on.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let endpoint = Endpoint {
            base_url: format!("http://127.0.0.1:{port}"),
            api_key: None,
            remote: false,
            host: "127.0.0.1".into(),
            port,
        };
        let (picker, env) = catalog_args(&endpoint, &[], false).await;
        assert!(picker.is_empty(), "{picker:?}");
        assert!(env.is_empty(), "{env:?}");

        // An empty catalog is the same shape of non-event: no lineup, and no
        // exports either (no row owns any alias).
        let base_url = spawn_models_mock(http::StatusCode::OK, r#"{"models":[]}"#.into()).await;
        let endpoint = Endpoint {
            base_url,
            ..endpoint
        };
        let (picker, env) = catalog_args(&endpoint, &[], false).await;
        assert!(picker.is_empty(), "{picker:?}");
        assert!(env.is_empty(), "{env:?}");
    }
}
