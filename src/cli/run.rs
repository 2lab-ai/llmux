//! `llmux run [-- args]` — ensure the proxy is running (auto-starting a
//! background daemon when needed), then spawn `claude` with the proxy env
//! injected and (unless opted out) the llmux model catalog injected into
//! Claude Code's `/model` picker.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::daemon::{ensure_server_running, EnsureOutcome};
use super::{resolve_endpoint, CliError, Endpoint, RunArgs};

/// Budget for the catalog fetch that feeds the `/model` picker. Deliberately
/// short and hard-capped: the picker is a convenience, so it must never delay
/// (let alone block) the `claude` launch.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(3);

/// One catalog row as the CLI consumes it — the CLI-side mirror of
/// [`crate::catalog::ModelEntry`] (which is `Serialize`-only, its `efforts`
/// being `&'static [&'static str]`). Only the fields the picker needs; unknown
/// keys are ignored so a newer daemon's richer rows still parse.
#[derive(Debug, Clone, Deserialize)]
struct CatalogRow {
    id: String,
    name: String,
    #[serde(default)]
    efforts: Vec<String>,
    #[serde(default)]
    max_context: Option<u64>,
    #[serde(default)]
    group: String,
}

/// `GET /llmux/models` response envelope.
#[derive(Debug, Deserialize)]
struct CatalogResponse {
    models: Vec<CatalogRow>,
}

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
/// Catalog order is preserved. `None` for an empty catalog — there is no
/// lineup to inject, and an empty `options` array would only add noise.
fn model_picker_settings(models: &[CatalogRow]) -> Option<String> {
    if models.is_empty() {
        return None;
    }
    let options = models
        .iter()
        .map(|row| PickerOption {
            model: row.id.clone(),
            label: row.name.clone(),
            description: row_description(row),
        })
        .collect();
    let settings = PickerSettings {
        model_picker: PickerLineup { options },
    };
    // A document of owned strings cannot fail to serialize.
    serde_json::to_string(&settings).ok()
}

/// The picker row's one-line description: backend group, the effort menu as a
/// `first…last` range, and the context window as a plain integer (no thousands
/// separators — the catalog's own figures read the same way in `docs/models.md`).
fn row_description(row: &CatalogRow) -> String {
    let mut parts = Vec::with_capacity(3);
    if !row.group.is_empty() {
        parts.push(row.group.clone());
    }
    match (row.efforts.first(), row.efforts.last()) {
        (Some(first), Some(last)) if first == last => parts.push(format!("efforts {first}")),
        (Some(first), Some(last)) => parts.push(format!("efforts {first}…{last}")),
        _ => {}
    }
    if let Some(ctx) = row.max_context {
        parts.push(format!("ctx {ctx}"));
    }
    parts.join(" · ")
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

/// Fetch the catalog from the proxy `claude` is about to be pointed at, with
/// the same client/header discipline as [`super::daemon::probe_server`]
/// (`x-api-key` when one is configured, both timeouts capped).
///
/// The error is a SANITIZED reason for a warning line: never the api key, never
/// the response body, never a raw transport error that could carry either.
async fn fetch_catalog(base_url: &str, api_key: Option<&str>) -> Result<Vec<CatalogRow>, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(CATALOG_TIMEOUT)
        .timeout(CATALOG_TIMEOUT)
        .build()
        .map_err(|_| "http client init failed".to_string())?;
    let mut request = client.get(format!("{base_url}/llmux/models"));
    if let Some(api_key) = api_key {
        request = request.header("x-api-key", api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|err| catalog_transport_reason(&err))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("catalog endpoint returned {status}"));
    }
    let body = response
        .text()
        .await
        .map_err(|_| "catalog response could not be read".to_string())?;
    serde_json::from_str::<CatalogResponse>(&body)
        .map(|doc| doc.models)
        .map_err(|_| "catalog response was not a llmux model document".to_string())
}

/// Classify a catalog-fetch transport failure into a fixed phrase — the
/// reqwest error's own `Display` is not used, so nothing from the request
/// (url, headers, body) can reach the warning line.
fn catalog_transport_reason(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "catalog fetch timed out".into()
    } else if err.is_connect() {
        "daemon not reachable for the catalog fetch".into()
    } else {
        "catalog fetch failed".into()
    }
}

/// The `--settings <json>` argv pair to PREPEND to the user's pass-through
/// args, or an empty vec when no lineup is injected. Every failure mode is
/// non-fatal: one warning line, then the launch continues unchanged.
async fn picker_args(endpoint: &Endpoint, claude_args: &[String], no_picker: bool) -> Vec<String> {
    if !injects_model_picker(claude_args, no_picker) {
        if !no_picker {
            eprintln!("warning: --settings given, llmux model picker lineup not injected");
        }
        return Vec::new();
    }
    let models = match fetch_catalog(&endpoint.base_url, endpoint.api_key.as_deref()).await {
        Ok(models) => models,
        Err(reason) => {
            eprintln!("warning: model picker not injected: {reason}");
            return Vec::new();
        }
    };
    match model_picker_settings(&models) {
        Some(json) => vec!["--settings".into(), json],
        None => {
            eprintln!("warning: model picker not injected: catalog is empty");
            Vec::new()
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

/// Local mode: ensure a server is listening (herdr-style auto-start: detached
/// daemon + readiness wait — see `cli::daemon`), then spawn `claude` with
/// `ANTHROPIC_BASE_URL=http://localhost:<port>` and pass-through args, and
/// propagate its exit code. Only `ANTHROPIC_BASE_URL` is set — Claude Code
/// keeps its own OAuth token (which the proxy accepts from localhost); not
/// setting `ANTHROPIC_API_KEY` keeps it in subscription mode.
///
/// Remote mode (`--remote` / `remote.host`): no local daemon is started;
/// `claude` is pointed at the remote proxy and `ANTHROPIC_API_KEY` is exported
/// with the remote's `x-api-key` so the off-loopback client-auth gate passes.
/// The proxy still replaces the client credential with the real upstream
/// account, so subscription mode is preserved at the account layer.
///
/// In both modes the catalog of the proxy being pointed at is fetched and
/// passed as `claude --settings '<modelPicker lineup>'` so `/model` lists the
/// llmux models (see [`model_picker_settings`]). Suppressed by
/// `--no-model-picker` or by the user's own `--settings`; a failed fetch is a
/// warning line, never a failed launch.
pub async fn run(args: RunArgs, remote: Option<String>) -> Result<(), CliError> {
    let config = crate::config::load_or_init()?;
    let endpoint = resolve_endpoint(remote.as_deref(), &config)?;

    // Remote mode: never auto-start a local daemon — point `claude` straight
    // at the remote proxy. Off-loopback the proxy enforces its `x-api-key`, so
    // we MUST export `ANTHROPIC_API_KEY` (the analogue of llmux-islands'
    // `x-api-key` header); the proxy still swaps in the real upstream account
    // credential, so the client key only unlocks the proxy's own gate.
    if endpoint.remote {
        if endpoint.api_key.is_none() {
            eprintln!(
                "warning: remote {}:{} has no api_key configured (set remote.api_key in \
                 ~/.config/llmux.json) — the proxy will reject the request unless it runs \
                 with no key",
                endpoint.host, endpoint.port
            );
        }
        eprintln!("using remote llmux at {}:{}", endpoint.host, endpoint.port);
    } else {
        match ensure_server_running(&config, args.force, None).await? {
            EnsureOutcome::Started { pid } => {
                eprintln!(
                    "started llmux server (pid {pid}) on port {}",
                    config.proxy.port
                );
            }
            EnsureOutcome::Restarted { pid } => {
                eprintln!(
                    "restarted llmux server (pid {pid}) on port {} → {}",
                    config.proxy.port,
                    crate::build_info::version_string()
                );
            }
            EnsureOutcome::AlreadyRunning => {}
        }
    }

    let mut claude_args = args.args.as_slice();
    if claude_args.first().map(String::as_str) == Some("--") {
        claude_args = &claude_args[1..];
    }

    // The picker lineup goes FIRST so the user's pass-through args still have
    // the last word on every other flag.
    let picker = picker_args(&endpoint, claude_args, args.no_model_picker).await;

    let (base_url, api_key, remove_key) = claude_env(&endpoint);
    let mut command = tokio::process::Command::new("claude");
    command
        .args(&picker)
        .args(claude_args)
        .env("ANTHROPIC_BASE_URL", &base_url);
    if let Some(key) = &api_key {
        command.env("ANTHROPIC_API_KEY", key);
    } else if remove_key {
        // Block a parent shell's unrelated upstream ANTHROPIC_API_KEY from
        // being inherited and leaking to the remote proxy over plain HTTP.
        command.env_remove("ANTHROPIC_API_KEY");
    }
    let status = command.status().await.map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            CliError::Message("claude not found in PATH — install Claude Code first".into())
        } else {
            CliError::Message(format!("failed to start claude: {err}"))
        }
    })?;

    std::process::exit(exit_code(&status));
}

/// Child exit code; signal terminations map to the conventional 128+N.
fn exit_code(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

#[cfg(test)]
mod tests {
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

    fn row(
        id: &str,
        name: &str,
        efforts: &[&str],
        max_context: Option<u64>,
        group: &str,
    ) -> CatalogRow {
        CatalogRow {
            id: id.into(),
            name: name.into(),
            efforts: efforts.iter().map(|e| (*e).to_string()).collect(),
            max_context,
            group: group.into(),
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
        let entries = crate::catalog::catalog("grok-4.6", "gpt-5.6-sol", "stealth/ox-alpha");
        let doc = serde_json::json!({ "models": entries }).to_string();
        let rows = serde_json::from_str::<CatalogResponse>(&doc)
            .expect("the served catalog parses as CLI catalog rows")
            .models;
        assert_eq!(rows.len(), entries.len());
        let json = model_picker_settings(&rows).unwrap();
        assert!(
            json.contains(r#"{"model":"claude-fable-5-1[1m]","label":"Claude Fable 5.1","description":"claude · efforts low…max · ctx 1000000"}"#),
            "{json}"
        );
        assert_eq!(
            json.matches(r#"{"model":"#).count(),
            entries.len(),
            "one picker row per catalog entry"
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

    /// Serve `body` with `status` at `/llmux/models` on 127.0.0.1:0.
    async fn spawn_models_mock(status: http::StatusCode, body: String) -> String {
        let app = axum::Router::new().route(
            "/llmux/models",
            axum::routing::get(move || async move { (status, body) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn fetch_catalog_reads_a_live_document() {
        let base_url = spawn_models_mock(
            http::StatusCode::OK,
            serde_json::json!({
                "models": [{
                    "id": "grok-4.6",
                    "aliases": ["grok"],
                    "name": "Grok 4.6",
                    "efforts": ["low", "high"],
                    "max_context": 500000u64,
                    "group": "grok",
                }]
            })
            .to_string(),
        )
        .await;
        let models = fetch_catalog(&base_url, Some("lm-key")).await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "grok-4.6");
        assert_eq!(models[0].max_context, Some(500_000));
    }

    /// A non-200 (or a body that is not a catalog) must return a sanitized
    /// Err — the warning path — and never panic, so the launch continues.
    #[tokio::test]
    async fn fetch_catalog_rejects_non_200_and_junk_bodies() {
        let base_url = spawn_models_mock(
            http::StatusCode::INTERNAL_SERVER_ERROR,
            "boom lm-secret-key".into(),
        )
        .await;
        let err = fetch_catalog(&base_url, Some("lm-secret-key"))
            .await
            .expect_err("non-200 must be an error");
        assert!(err.contains("500"), "{err}");
        assert!(!err.contains("lm-secret-key"), "leaked credential: {err}");
        assert!(!err.contains("boom"), "leaked body: {err}");

        let base_url = spawn_models_mock(http::StatusCode::OK, "<html>hello</html>".into()).await;
        let err = fetch_catalog(&base_url, None)
            .await
            .expect_err("a non-catalog body must be an error");
        assert!(err.contains("not a llmux model document"), "{err}");
    }

    #[tokio::test]
    async fn fetch_catalog_reports_an_unreachable_daemon() {
        // Bind then drop to reserve-and-free a port nobody listens on.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let err = fetch_catalog(&format!("http://127.0.0.1:{port}"), None)
            .await
            .expect_err("nothing listening must be an error");
        assert!(err.contains("not reachable"), "{err}");
    }
}
