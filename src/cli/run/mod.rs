//! `llmux run [--codex] [-- args]` — ensure the proxy is running
//! (auto-starting a background daemon when needed), then launch the selected
//! client frontend pointed at it and propagate its exit code.
//!
//! The shared launcher lives here: endpoint resolution, the daemon lifecycle,
//! the `--` strip, the one catalog fetch (`GET /llmux/models`), the private
//! session-file helper and the spawn/exit-code plumbing. Each client keeps
//! only what differs in its own module and is selected by one [`Frontend`]
//! value:
//!
//! - [`claude`] — Claude Code (`claude`, Anthropic Messages ingress): the
//!   `/model` picker lineup as a `--settings` document, the catalog's alias
//!   owners as `ANTHROPIC_DEFAULT_*_MODEL` exports, the endpoint env and the
//!   llmux custom headers.
//! - [`codex`] — Codex CLI (`codex`, OpenAI Responses ingress): the
//!   session-scoped `-c` provider flags and the Codex model catalog file.
//!
//! A new client frontend is one more [`Frontend`] case, one more module with
//! a `launch`, and one more arm in [`run`].

mod claude;
mod codex;

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::daemon::{ensure_server_running, EnsureOutcome};
use super::{resolve_endpoint, CliError, EnvArgs, RunArgs};

/// The client frontend a `run` (or an `env`) targets. What differs per client
/// at the launcher level is answered here; each client's launch lives in its
/// own module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Frontend {
    /// Claude Code over the Anthropic Messages ingress (`/v1/messages`).
    ClaudeCode,
    /// Codex CLI over the OpenAI Responses ingress (`/v1/responses`).
    Codex,
}

impl Frontend {
    /// The frontend `llmux run` launches: `--codex` selects Codex, no flag
    /// Claude Code. The flags share one clap group in [`RunArgs`], so at most
    /// one is ever set.
    pub(crate) fn from_run_args(args: &RunArgs) -> Frontend {
        if args.codex {
            Frontend::Codex
        } else {
            Frontend::ClaudeCode
        }
    }

    /// The frontend whose exports `llmux env` prints.
    pub(crate) fn from_env_args(args: &EnvArgs) -> Frontend {
        if args.codex {
            Frontend::Codex
        } else {
            Frontend::ClaudeCode
        }
    }

    /// The client binary looked up in `PATH`.
    pub(crate) fn binary(self) -> &'static str {
        match self {
            Frontend::ClaudeCode => "claude",
            Frontend::Codex => "codex",
        }
    }

    /// The product named in the "not found in PATH" message.
    pub(crate) fn product(self) -> &'static str {
        match self {
            Frontend::ClaudeCode => "Claude Code",
            Frontend::Codex => "Codex CLI",
        }
    }

    /// The launch failure for `err`: a missing binary names the product to
    /// install, anything else is reported as a failed start.
    fn spawn_error(self, err: &std::io::Error) -> CliError {
        let binary = self.binary();
        if err.kind() == std::io::ErrorKind::NotFound {
            CliError::Message(format!(
                "{binary} not found in PATH — install {} first",
                self.product()
            ))
        } else {
            CliError::Message(format!("failed to start {binary}: {err}"))
        }
    }
}

/// Budget for the catalog fetch that feeds the client's model picker.
/// Deliberately short and hard-capped: the picker is a convenience, so it must
/// never delay (let alone block) the client launch.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(3);

/// One catalog row as the CLI consumes it — the CLI-side mirror of
/// [`crate::catalog::ModelEntry`] (which is `Serialize`-only, its `efforts`
/// being `&'static [&'static str]`). Only the fields the pickers need; unknown
/// keys are ignored so a newer daemon's richer rows still parse.
#[derive(Debug, Clone, Deserialize)]
struct CatalogRow {
    id: String,
    /// Extra request slugs that resolve to this id — the source of the
    /// `ANTHROPIC_DEFAULT_*_MODEL` exports (see [`claude::alias_env`]) and of
    /// the hidden Codex alias entries.
    #[serde(default)]
    aliases: Vec<String>,
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

/// A picker row's one-line description: backend group, the effort menu as a
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

/// Fetch the catalog from the proxy the client is about to be pointed at,
/// with the same client/header discipline as [`super::daemon::probe_server`]
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

/// A private file handed to the client for one session: created with
/// `create_new` (never follows or reuses an existing path) and mode `0600`
/// under the temp dir, and removed again when dropped — [`spawn`] drops it
/// right after the child exits, before the process exit that would skip
/// destructors.
struct SessionFile {
    path: PathBuf,
}

impl SessionFile {
    /// Create `<temp>/<prefix>-<ulid>.json` holding `contents`. A failed write
    /// removes the half-written file before the error is returned.
    fn create(prefix: &str, contents: &[u8]) -> std::io::Result<SessionFile> {
        let path = std::env::temp_dir().join(format!("{prefix}-{}.json", ulid::Ulid::new()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            use std::io::Write as _;
            let mut f = options.open(&path)?;
            f.write_all(contents)
        })();
        match result {
            Ok(()) => Ok(SessionFile { path }),
            Err(err) => {
                let _ = std::fs::remove_file(&path);
                Err(err)
            }
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SessionFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Run the client to completion, remove its session files, and exit with its
/// exit code. Only a failed start returns (as the frontend's launch error).
async fn spawn(
    mut command: tokio::process::Command,
    frontend: Frontend,
    session_files: Vec<SessionFile>,
) -> Result<Infallible, CliError> {
    let status = command.status().await;
    drop(session_files);
    let status = status.map_err(|err| frontend.spawn_error(&err))?;
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

/// Local mode: ensure a server is listening (herdr-style auto-start: detached
/// daemon + readiness wait — see `cli::daemon`), then launch the selected
/// client against `http://localhost:<port>` with the pass-through args, and
/// propagate its exit code.
///
/// Remote mode (`--remote` / `remote.host`): no local daemon is started; the
/// client is pointed at the remote proxy with the remote's key so the
/// off-loopback client-auth gate passes. The proxy still replaces the client
/// credential with the real upstream account, so subscription mode is
/// preserved at the account layer.
///
/// What each client is handed lives in its module ([`claude::launch`],
/// [`codex::launch`]); in both the catalog of the proxy being pointed at is
/// fetched at most ONCE, `--no-model-picker` suppresses it, and a failed fetch
/// is a warning line, never a failed launch.
pub async fn run(args: RunArgs, remote: Option<String>) -> Result<(), CliError> {
    let config = crate::config::load_or_init()?;
    let endpoint = resolve_endpoint(remote.as_deref(), &config)?;

    // Remote mode: never auto-start a local daemon — point the client straight
    // at the remote proxy. Off-loopback the proxy enforces its `x-api-key`, so
    // the client MUST present the remote's key (the analogue of llmux-islands'
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

    let mut client_args = args.args.as_slice();
    if client_args.first().map(String::as_str) == Some("--") {
        client_args = &client_args[1..];
    }

    let launched = match Frontend::from_run_args(&args) {
        Frontend::ClaudeCode => claude::launch(&endpoint, client_args, args.no_model_picker).await,
        Frontend::Codex => {
            codex::launch(
                &endpoint,
                client_args,
                args.no_model_picker,
                &config.codex.default_model,
            )
            .await
        }
    };
    match launched? {}
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn row(
        id: &str,
        name: &str,
        efforts: &[&str],
        max_context: Option<u64>,
        group: &str,
    ) -> CatalogRow {
        CatalogRow {
            id: id.into(),
            aliases: Vec::new(),
            name: name.into(),
            efforts: efforts.iter().map(|e| (*e).to_string()).collect(),
            max_context,
            group: group.into(),
        }
    }

    /// Serve `body` with `status` at `/llmux/models` on 127.0.0.1:0.
    pub(super) async fn spawn_models_mock(status: http::StatusCode, body: String) -> String {
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
        // `aliases` is parsed, not ignored — the alias exports read it.
        assert_eq!(models[0].aliases, vec!["grok".to_string()]);
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

    /// The `run` flags pick the frontend; the clap surface is unchanged
    /// (`--codex`, nothing for Claude Code).
    #[test]
    fn frontend_follows_the_run_and_env_flags() {
        use clap::Parser as _;
        let parse = |argv: &[&str]| match crate::cli::Cli::try_parse_from(argv)
            .expect("parses")
            .command
        {
            crate::cli::Command::Run(args) => Frontend::from_run_args(&args),
            crate::cli::Command::Env(args) => Frontend::from_env_args(&args),
            other => panic!("unexpected command {other:?}"),
        };
        assert_eq!(parse(&["llmux", "run"]), Frontend::ClaudeCode);
        assert_eq!(parse(&["llmux", "run", "--codex"]), Frontend::Codex);
        assert_eq!(
            parse(&["llmux", "run", "--", "--codex"]),
            Frontend::ClaudeCode,
            "a pass-through --codex belongs to the client"
        );
        assert_eq!(parse(&["llmux", "env"]), Frontend::ClaudeCode);
        assert_eq!(parse(&["llmux", "env", "--codex"]), Frontend::Codex);
    }

    /// The launch failures read exactly as the two per-client messages did.
    #[test]
    fn spawn_error_names_the_binary_and_the_product() {
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let message = |frontend: Frontend, err: &std::io::Error| match frontend.spawn_error(err) {
            CliError::Message(message) => message,
            other => panic!("unexpected error {other:?}"),
        };
        assert_eq!(
            message(Frontend::ClaudeCode, &missing),
            "claude not found in PATH — install Claude Code first"
        );
        assert_eq!(
            message(Frontend::Codex, &missing),
            "codex not found in PATH — install Codex CLI first"
        );
        assert_eq!(
            message(Frontend::ClaudeCode, &denied),
            format!("failed to start claude: {denied}")
        );
        assert_eq!(
            message(Frontend::Codex, &denied),
            format!("failed to start codex: {denied}")
        );
    }

    /// A session file is private to the user, holds exactly its contents, and
    /// is gone once dropped.
    #[test]
    fn session_file_is_private_and_removed_on_drop() {
        let file = SessionFile::create("llmux-test-session", b"{\"models\":[]}").unwrap();
        let path = file.path().to_path_buf();
        assert!(path.starts_with(std::env::temp_dir()), "{path:?}");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("llmux-test-session-") && name.ends_with(".json"),
            "{name}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"models\":[]}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        drop(file);
        assert!(!path.exists(), "removed on drop");
    }
}
