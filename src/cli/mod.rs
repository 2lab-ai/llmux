//! CLI surface and user-facing argument contract. Command handlers live in
//! their matching modules.

pub mod accounts;
pub mod api;
pub mod brew;
pub mod channel;
pub mod daemon;
pub mod env;
pub mod import;
pub mod login;
pub mod run;
pub mod status;
pub mod update;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    #[error(transparent)]
    Auth(#[from] crate::auth::AuthError),
    #[error(transparent)]
    Proxy(#[from] crate::proxy::ProxyError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Message(String),
}

/// After-help block documenting remote mode (the config snippet, examples and
/// the command matrix) — shown by `llmux --help`.
const REMOTE_HELP: &str = "\
Remote daemon:
  Drive one central daemon (e.g. llmux-host:3456) from many client machines.
  Turn on remote mode with `--remote host[:port]`, or persistently in
  ~/.config/llmux.json (api_key = the REMOTE daemon's proxy.api_key):

      { \"remote\": { \"host\": \"llmux-host\", \"port\": 3456, \"api_key\": \"lm-…\" } }

  Examples:
      llmux --remote llmux-host run     # point claude at the remote proxy
      llmux status                       # probe the remote (with remote.host set)

  In remote mode run/server/dashboard/status/env/accounts target the remote;
  stop/restart/remove/login/import are refused (run them on the daemon's host);
  channel/update stay local (they manage this machine's binary).

  Transport is plain HTTP — use over a trusted, encrypted overlay
  (Tailscale / WireGuard) ONLY. Ownership is not encryption.";

#[derive(Debug, Parser)]
#[command(
    name = "llmux",
    version = crate::build_info::version_with_build(),
    about = "Multi-account LLM proxy for Claude Code with quota-maximizing scheduling",
    after_long_help = REMOTE_HELP
)]
pub struct Cli {
    /// Target a remote llmux daemon (`host` or `host:port`) instead of the
    /// local one, for this invocation. Overrides `remote.host` in the config;
    /// `:port` defaults to `remote.port` (else 3456), api_key from
    /// `remote.api_key`. In remote mode `run`/`server`/`dashboard`/`status`/
    /// `env`/`accounts` target the remote, while `stop`/`restart`/`remove`/
    /// `login`/`import` are refused (they belong on the daemon's own host) and
    /// `channel`/`update` stay local. See `llmux --help` for a config example.
    #[arg(long, global = true, value_name = "HOST[:PORT]")]
    pub remote: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the proxy (TUI dashboard on a TTY, plain logs otherwise).
    Server(ServerArgs),
    /// Spawn `claude` with ANTHROPIC_BASE_URL pointed at the proxy
    /// (auto-starts the server as a background daemon when needed).
    Run(RunArgs),
    /// Stop a running server (POST /llmux/shutdown, wait for the port
    /// to free).
    Stop(StopArgs),
    /// Restart the daemon: cooperatively drain a running server (if any),
    /// then spawn this binary's version. Does not exec `claude`.
    Restart(RestartArgs),
    /// Add an account via browser OAuth (or paste an API key with --api).
    Login(LoginArgs),
    /// Import accounts from teamclaude config, ~/.claude/.credentials.json,
    /// or inline JSON.
    Import(ImportArgs),
    /// Print the env exports for pointing Claude Code at the proxy.
    Env(EnvArgs),
    /// Attach to a running daemon and render its dashboard (read-only except
    /// manual switch). Polls `GET /llmux/dashboard` over HTTP.
    Dashboard(DashboardArgs),
    /// Show scheduler/account state (from a running server when available).
    ///
    /// Exit codes: 0 = server running, 1 = server not running (or error).
    Status(StatusArgs),
    /// List configured accounts.
    Accounts(AccountsArgs),
    /// Remove an account by name.
    Remove(RemoveArgs),
    /// Debug: perform a GET against the upstream API on the current account.
    Api(ApiArgs),
    /// Print the current release channel, or switch it (`stable`/`preview`).
    ///
    /// The channel is derived from what Homebrew has installed — there is no
    /// config field. Switching runs the brew install/uninstall dance, mirrors
    /// it onto the Islands cask, and restarts a running daemon.
    Channel(ChannelArgs),
    /// Self-update on the current channel (`brew upgrade`), then restart the
    /// daemon / relaunch Islands only if their versions actually changed.
    Update(UpdateArgs),
}

#[derive(Debug, Args)]
pub struct ServerArgs {
    /// Override the configured listen port.
    #[arg(long)]
    pub port: Option<u16>,
    /// Force plain log output even on a TTY (no TUI).
    #[arg(long)]
    pub no_tui: bool,
    /// Write one log file per proxied request into DIR (credentials masked).
    #[arg(long, value_name = "DIR")]
    pub log_to: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Restart the daemon even when it already runs this binary's version
    /// (by default a same-version daemon is reused; a different version is
    /// always restarted).
    #[arg(long)]
    pub force: bool,
    /// Arguments passed through to `claude` after `--`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct StopArgs {}

#[derive(Debug, Args)]
pub struct RestartArgs {}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Add a manual API key instead of running the OAuth browser flow.
    #[arg(long)]
    pub api: bool,
    /// Add an OpenAI Codex (ChatGPT subscription) account via the ChatGPT
    /// OAuth browser flow instead of the Claude flow.
    #[arg(long, conflicts_with = "api")]
    pub codex: bool,
    /// Add an xAI Grok subscription account via the device-code flow
    /// (opens a verification page; no localhost callback).
    #[arg(long, conflicts_with_all = ["api", "codex"])]
    pub grok: bool,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Path to a teamclaude config or a ~/.claude/.credentials.json file.
    /// Defaults to probing both well-known locations.
    #[arg(long)]
    pub from: Option<PathBuf>,
    /// Inline JSON credential blob (single account or array).
    #[arg(long, conflicts_with = "from")]
    pub json: Option<String>,
}

#[derive(Debug, Args)]
pub struct EnvArgs {}

#[derive(Debug, Args)]
pub struct DashboardArgs {}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Emit raw JSON instead of the human-readable table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AccountsArgs {
    /// Include window/cooldown detail per account.
    #[arg(short, long)]
    pub verbose: bool,
    /// Emit the full live account dashboard as JSON — the currently selected
    /// subscription per group plus every account's 5h/7d usage windows,
    /// resets, status, in-flight count and token health — sourced from the
    /// running server. Exits 1 (with a JSON error object) if no server is up.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// Account name as shown by `llmux accounts`.
    pub name: String,
    /// Skip the confirmation prompt (required when stdin is not a TTY).
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct ApiArgs {
    /// Upstream path to GET (e.g. /api/oauth/usage).
    pub path: String,
}

#[derive(Debug, Args)]
pub struct ChannelArgs {
    /// Target channel to switch to. Omit to print the current channel.
    #[arg(value_enum)]
    pub channel: Option<ChannelName>,
}

/// The user-facing channel names accepted by `llmux channel <name>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ChannelName {
    Stable,
    Preview,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {}

/// Dispatch a parsed CLI invocation to its handler.
///
/// Tracing init lives here (not in `main`): the server command chooses its
/// own subscriber once it knows whether the TUI owns the terminal; every
/// other command logs plainly to stderr.
pub async fn dispatch(cli: Cli) -> Result<(), CliError> {
    // `server` and `dashboard` are (potentially) TUI commands: they pick
    // their own subscriber once they know whether ratatui owns the terminal.
    // Nothing else may write to the terminal under a live TUI.
    if !matches!(cli.command, Command::Server(_) | Command::Dashboard(_)) {
        crate::logging::init_plain();
    }
    let remote = cli.remote;

    // Refuse guard (remote-CLI design rule): in remote mode a command either
    // TARGETS the remote or REFUSES loudly — silently operating on the local
    // daemon is the defect class this forbids. Lifecycle (`stop`/`restart`) and
    // account-mutation (`remove`/`login`/`import`) commands are meaningless
    // against a pure client, so reject them with a message that names the
    // remote. `channel`/`update` are deliberately NOT here — they manage this
    // machine's binary install, so they stay local even in remote mode.
    if let Some(cmd) = remote_refused_command(&cli.command) {
        let config = crate::config::load_or_init()?;
        let endpoint = resolve_endpoint(remote.as_deref(), &config)?;
        if endpoint.remote {
            return Err(refuse_remote(cmd, &endpoint));
        }
    }

    match cli.command {
        Command::Server(args) => server(args, remote).await,
        Command::Run(args) => run::run(args, remote).await,
        Command::Stop(args) => daemon::stop(args).await,
        Command::Restart(_) => daemon::restart(None).await,
        Command::Login(args) => login::run(args).await,
        Command::Import(args) => import::run(args).await,
        Command::Env(args) => env::run(args, remote).await,
        Command::Dashboard(args) => dashboard(args, remote).await,
        Command::Status(args) => status::run(args, remote).await,
        Command::Accounts(args) => accounts::list(args, remote).await,
        Command::Remove(args) => accounts::remove(args).await,
        Command::Api(args) => api::run(args).await,
        Command::Channel(args) => channel::run(args).await,
        Command::Update(args) => update::run(args).await,
    }
}

/// `llmux server` — start the proxy, rendering the in-process TUI on a
/// TTY (unless `--no-tui`).
///
/// herdr semantics (the daemon owns port 3456 and the only local TUI): before
/// touching the terminal we probe the port.
/// - A llmux daemon already runs → print one line and enter the SAME
///   attach mode `llmux dashboard` uses (read-only dashboard over HTTP).
/// - A FOREIGN process answers the port → clean one-line error, NO TUI init.
/// - Nothing is listening → bind FIRST (via `serve`'s readiness signal), and
///   only after the bind succeeds initialize the TUI, so a bind error can
///   never paint over a half-initialized frame again.
async fn server(args: ServerArgs, remote: Option<String>) -> Result<(), CliError> {
    use std::io::IsTerminal as _;

    let mut config = crate::config::load_or_init()?;
    if let Some(port) = args.port {
        config.proxy.port = port;
    }
    let use_tui = !args.no_tui && std::io::stdout().is_terminal() && std::io::stdin().is_terminal();

    // Remote mode: `llmux server` never binds a local proxy — it attaches to
    // the configured remote daemon and renders its dashboard (the CLI twin of
    // llmux-islands). This is the whole point of `--remote` / `remote.host`.
    let endpoint = resolve_endpoint(remote.as_deref(), &config)?;
    if endpoint.remote {
        return dashboard_endpoint(endpoint, use_tui).await;
    }

    // herdr: is someone already on the port? (Cheap HTTP probe — no terminal
    // touched yet, so a foreign-process error stays a clean stderr line.)
    let port = config.proxy.port;
    let api_key = config.proxy.api_key.clone();
    match daemon::probe_server(&proxy_base_url(port), api_key.as_deref()).await? {
        daemon::ServerProbe::Running { status } => {
            let pid = daemon::status_pid(&status);
            let pid_str = pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into());
            eprintln!("daemon already running (pid {pid_str}) — attaching…");
            // No bind, no plain-log init: hand straight to the attach TUI
            // (or, with --no-tui, a one-liner so scripts don't hang).
            if !use_tui {
                eprintln!(
                    "a llmux daemon already owns port {port}; run `llmux dashboard` to attach"
                );
                return Ok(());
            }
            return attach(proxy_base_url(port), api_key, pid).await;
        }
        daemon::ServerProbe::Unauthorized => {
            return Err(CliError::Message(format!(
                "a llmux daemon on port {port} rejected the local api key (401) — \
                 check proxy.api_key in the config"
            )));
        }
        daemon::ServerProbe::Foreign { detail } => {
            return Err(CliError::Message(format!(
                "port {port} is in use by something that is not llmux ({detail})\n\
                 Free the port or change proxy.port in the config."
            )));
        }
        daemon::ServerProbe::NotRunning => {}
    }

    // An empty account list is NOT an error: the server (and its TUI / the
    // llmux-islands app) come up so the user can add accounts from there
    // (the `+` button → OAuth login, or `llmux login`). The scheduler simply
    // has an empty pool until the first account lands — every downstream path
    // (status/dashboard/islands) already tolerates zero accounts.

    // Tracing routing is decided before anything can log. TUI mode: the ONLY
    // output is the channel bridge into the log console pane — nothing may
    // write to the terminal except ratatui. Lines emitted before the first
    // draw just wait in the channel.
    let logs_rx = if use_tui {
        Some(crate::logging::init_tui_bridge())
    } else {
        crate::logging::init_plain();
        None
    };

    let pool = crate::scheduler::AccountPool::new(&config.accounts);
    let logger = match &args.log_to {
        Some(dir) => Some(std::sync::Arc::new(
            crate::proxy::logging::RequestLogger::new(dir.clone())
                .map_err(crate::proxy::ProxyError::from)?,
        )),
        None => None,
    };
    let state = crate::proxy::server::AppState::new(config, pool, logger, logs_rx)
        .map_err(CliError::Proxy)?;

    if !use_tui {
        // No TUI: serve in the foreground; the fold task re-traces activity
        // events into stderr (daemon parity).
        crate::proxy::server::serve(state, None).await?;
        return Ok(());
    }

    // TUI mode: bind BEFORE initializing the terminal. `serve` reports the
    // bound address on `ready`; if it fails to bind it returns the error
    // first, so we never call `ratatui::try_init` over a doomed server.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let tui_state = state.clone();
    let mut serve_task = tokio::spawn(crate::proxy::server::serve(state, Some(ready_tx)));
    tokio::select! {
        bound = ready_rx => {
            // `Err` means `serve` dropped the sender (it returned before
            // binding) — surface its error, not a generic "channel closed".
            if bound.is_err() {
                return match serve_task.await {
                    Ok(result) => result.map_err(CliError::Proxy),
                    Err(join) => Err(CliError::Message(format!("server task panicked: {join}"))),
                };
            }
        }
        result = &mut serve_task => {
            return match result {
                Ok(result) => result.map_err(CliError::Proxy),
                Err(join) => Err(CliError::Message(format!("server task panicked: {join}"))),
            };
        }
    }

    // Bind confirmed — now it is safe to own the terminal. Whichever side
    // finishes first (TUI quit, server error) ends the process.
    tokio::select! {
        result = crate::tui::run_local(tui_state) => result?,
        result = &mut serve_task => match result {
            Ok(result) => result?,
            Err(join) => return Err(CliError::Message(format!("server task panicked: {join}"))),
        },
    }
    Ok(())
}

/// `llmux dashboard` — attach to a running daemon (local, or the configured
/// `--remote` / `remote.host`) and render its dashboard. Refuses cleanly when
/// no daemon (or a foreign process) is on the endpoint — there is nothing
/// local to fall back to here.
async fn dashboard(_args: DashboardArgs, remote: Option<String>) -> Result<(), CliError> {
    let config = crate::config::load_or_init()?;
    let endpoint = resolve_endpoint(remote.as_deref(), &config)?;
    let use_tui = std::io::IsTerminal::is_terminal(&std::io::stdout());
    dashboard_endpoint(endpoint, use_tui).await
}

/// Probe an already-resolved endpoint and attach to it (shared by
/// `llmux dashboard` and `llmux server` in remote mode). `use_tui` gates the
/// interactive attach: without a TTY we print a one-liner instead of hanging.
async fn dashboard_endpoint(endpoint: Endpoint, use_tui: bool) -> Result<(), CliError> {
    let Endpoint {
        base_url,
        api_key,
        host,
        port,
        ..
    } = endpoint;
    match daemon::probe_server(&base_url, api_key.as_deref()).await? {
        daemon::ServerProbe::Running { status } => {
            if !use_tui {
                eprintln!("llmux daemon reachable at {host}:{port}; run `llmux dashboard` on a TTY to attach");
                return Ok(());
            }
            attach(base_url, api_key, daemon::status_pid(&status)).await
        }
        daemon::ServerProbe::NotRunning => Err(CliError::Message(format!(
            "no llmux daemon at {host}:{port} — check the host/port and that the remote server is up"
        ))),
        daemon::ServerProbe::Unauthorized => Err(CliError::Message(format!(
            "authentication failed at {host}:{port} (401) — set or fix `remote.api_key` in \
             ~/.config/llmux.json (it must be the remote daemon's proxy.api_key)"
        ))),
        daemon::ServerProbe::Foreign { detail } => Err(CliError::Message(format!(
            "{host}:{port} is in use by something that is not llmux ({detail})"
        ))),
    }
}

/// Enter attach mode against a confirmed llmux daemon: the remote TUI
/// polls `GET /llmux/dashboard` and renders the identical layout. No
/// tracing subscriber is installed (ratatui owns the terminal; the client has
/// no logs of its own to show).
async fn attach(
    base_url: String,
    api_key: Option<String>,
    pid: Option<u32>,
) -> Result<(), CliError> {
    let opts = crate::tui::RemoteOptions {
        base_url,
        api_key,
        pid,
    };
    crate::tui::run_remote(opts).await?;
    Ok(())
}

/// `http://localhost:<port>` — the whole Claude Code integration contract for
/// the LOCAL daemon.
pub(crate) fn proxy_base_url(port: u16) -> String {
    format!("http://localhost:{port}")
}

/// `http://<host>:<port>` — base URL for an arbitrary (local or remote) daemon.
pub(crate) fn base_url(host: &str, port: u16) -> String {
    format!("http://{host}:{port}")
}

/// A resolved client endpoint: where to reach the proxy, the key to present,
/// and whether it is the LOCAL loopback daemon (which the CLI may auto-start /
/// stop) or a REMOTE one (which the CLI only talks to over HTTP and never
/// manages).
pub(crate) struct Endpoint {
    pub base_url: String,
    pub api_key: Option<String>,
    pub remote: bool,
    pub host: String,
    pub port: u16,
}

/// Which commands are refused in remote mode, and their display name. `Some` =
/// refuse (lifecycle `stop`/`restart` + account-mutation `remove`/`login`/
/// `import` — all act on the LOCAL daemon/config, meaningless on a pure client);
/// `None` = allowed (either targets the remote, or — `channel`/`update` — stays
/// local by design). Pure over the command so the refuse SET is unit-testable;
/// this is the mechanization of the "target the remote or refuse loudly" rule.
fn remote_refused_command(command: &Command) -> Option<&'static str> {
    match command {
        Command::Stop(_) => Some("stop"),
        Command::Restart(_) => Some("restart"),
        Command::Remove(_) => Some("remove"),
        Command::Login(_) => Some("login"),
        Command::Import(_) => Some("import"),
        _ => None,
    }
}

/// The error for a command that is refused in remote mode: lifecycle
/// (`stop`/`restart`) and account-mutation (`remove`/`login`/`import`) commands
/// act on the LOCAL daemon/config, which is meaningless on a pure client. This
/// is the single message shape for all of them — it names the remote so the
/// user knows where the command belongs, and refuses LOUDLY instead of silently
/// touching a local daemon (the defect class the remote-CLI design forbids).
pub(crate) fn refuse_remote(cmd: &str, endpoint: &Endpoint) -> CliError {
    CliError::Message(format!(
        "`llmux {cmd}` acts on the LOCAL daemon/config and is refused in remote mode \
         (targeting {}:{}) — run it on the daemon's host, or drop `--remote` / unset \
         `remote.host` to act locally",
        endpoint.host, endpoint.port
    ))
}

/// Resolve the effective client endpoint from the `--remote` flag and the
/// config, in that precedence:
///
/// 1. `--remote host[:port]` flag → remote (port defaults to the config's
///    `remote.port`, else 3456; api_key from `remote.api_key`).
/// 2. `remote.host` set in the config → remote (port/api_key from `remote`).
/// 3. neither → LOCAL loopback on `proxy.port` (api_key from `proxy.api_key`).
///
/// A `--remote` value with an empty host is rejected. This is the single
/// chokepoint every client command routes through, so local and remote share
/// one code path.
pub(crate) fn resolve_endpoint(
    remote_flag: Option<&str>,
    config: &crate::config::Config,
) -> Result<Endpoint, CliError> {
    if let Some(spec) = remote_flag {
        let (host, port) = parse_remote_spec(spec, config.remote.port)?;
        return Ok(Endpoint {
            base_url: base_url(&host, port),
            api_key: config.remote.api_key.clone(),
            remote: true,
            host,
            port,
        });
    }
    if let Some(host) = config.remote.host.clone() {
        if host.trim().is_empty() {
            return Err(CliError::Message(
                "remote.host in the config is empty — set a host or remove the remote section"
                    .into(),
            ));
        }
        let port = config.remote.port.unwrap_or(crate::config::DEFAULT_PORT);
        return Ok(Endpoint {
            base_url: base_url(&host, port),
            api_key: config.remote.api_key.clone(),
            remote: true,
            host,
            port,
        });
    }
    Ok(Endpoint {
        base_url: proxy_base_url(config.proxy.port),
        api_key: config.proxy.api_key.clone(),
        remote: false,
        host: "localhost".into(),
        port: config.proxy.port,
    })
}

/// Parse `host` or `host:port` from a `--remote` value. `default_port` (the
/// config's `remote.port`) is used when no `:port` is given, falling back to
/// 3456.
fn parse_remote_spec(spec: &str, default_port: Option<u16>) -> Result<(String, u16), CliError> {
    let spec = spec.trim();
    let fallback = default_port.unwrap_or(crate::config::DEFAULT_PORT);
    let (host, port) = match spec.rsplit_once(':') {
        Some((host, port_str)) => {
            let port = port_str.parse::<u16>().map_err(|_| {
                CliError::Message(format!(
                    "invalid --remote port in '{spec}' (expected a number)"
                ))
            })?;
            (host, port)
        }
        None => (spec, fallback),
    };
    if host.is_empty() {
        return Err(CliError::Message(format!(
            "invalid --remote '{spec}' — expected host or host:port"
        )));
    }
    Ok((host.to_string(), port))
}

/// Print `prompt` on stderr and read one trimmed line from stdin.
pub(crate) fn prompt_line(prompt: &str) -> Result<String, CliError> {
    use std::io::Write as _;

    let mut stderr = std::io::stderr();
    write!(stderr, "{prompt}")?;
    stderr.flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Wall clock as epoch milliseconds (saturating at 0 before the epoch).
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, RemoteConfig};

    #[test]
    fn parse_remote_spec_host_only_uses_default_port() {
        assert_eq!(
            parse_remote_spec("llmux-host", Some(4000)).unwrap(),
            ("llmux-host".to_string(), 4000)
        );
        // No configured remote.port → 3456.
        assert_eq!(
            parse_remote_spec("llmux-host", None).unwrap(),
            ("llmux-host".to_string(), crate::config::DEFAULT_PORT)
        );
    }

    #[test]
    fn parse_remote_spec_host_and_port() {
        assert_eq!(
            parse_remote_spec("llmux-host:3456", None).unwrap(),
            ("llmux-host".to_string(), 3456)
        );
        assert_eq!(
            parse_remote_spec("100.64.0.1:9000", None).unwrap(),
            ("100.64.0.1".to_string(), 9000)
        );
    }

    #[test]
    fn parse_remote_spec_rejects_bad_input() {
        assert!(parse_remote_spec("llmux-host:notaport", None).is_err());
        assert!(parse_remote_spec(":3456", None).is_err());
        assert!(parse_remote_spec("", None).is_err());
    }

    #[test]
    fn resolve_endpoint_defaults_to_local() {
        let mut config = Config::default();
        config.proxy.port = 7777;
        config.proxy.api_key = Some("lm-local".into());
        let ep = resolve_endpoint(None, &config).unwrap();
        assert!(!ep.remote);
        assert_eq!(ep.base_url, "http://localhost:7777");
        assert_eq!(ep.api_key.as_deref(), Some("lm-local"));
    }

    #[test]
    fn resolve_endpoint_flag_overrides_config() {
        let config = Config {
            remote: RemoteConfig {
                host: Some("configured-host".into()),
                port: Some(1111),
                api_key: Some("lm-remote".into()),
            },
            ..Config::default()
        };
        // Flag host wins; port from flag; api_key still from remote config.
        let ep = resolve_endpoint(Some("llmux-host:3456"), &config).unwrap();
        assert!(ep.remote);
        assert_eq!(ep.base_url, "http://llmux-host:3456");
        assert_eq!(ep.api_key.as_deref(), Some("lm-remote"));
    }

    #[test]
    fn resolve_endpoint_flag_host_only_uses_remote_port() {
        let config = Config {
            remote: RemoteConfig {
                port: Some(2222),
                ..RemoteConfig::default()
            },
            ..Config::default()
        };
        let ep = resolve_endpoint(Some("llmux-host"), &config).unwrap();
        assert_eq!(ep.base_url, "http://llmux-host:2222");
    }

    #[test]
    fn resolve_endpoint_config_remote_host() {
        let config = Config {
            remote: RemoteConfig {
                host: Some("llmux-host".into()),
                port: None,
                api_key: Some("lm-remote".into()),
            },
            ..Config::default()
        };
        let ep = resolve_endpoint(None, &config).unwrap();
        assert!(ep.remote);
        // No remote.port → DEFAULT_PORT.
        assert_eq!(
            ep.base_url,
            format!("http://llmux-host:{}", crate::config::DEFAULT_PORT)
        );
        assert_eq!(ep.api_key.as_deref(), Some("lm-remote"));
    }

    #[test]
    fn remote_refused_command_covers_lifecycle_and_account_mutation() {
        // The exact refuse SET — lifecycle + account mutation. Guarding this is
        // the defect-class prevention (never silently touch a local daemon in
        // remote mode), so pin every member.
        assert_eq!(
            remote_refused_command(&Command::Stop(StopArgs {})),
            Some("stop")
        );
        assert_eq!(
            remote_refused_command(&Command::Restart(RestartArgs {})),
            Some("restart")
        );
        assert_eq!(
            remote_refused_command(&Command::Remove(RemoveArgs {
                name: "acct".into(),
                yes: true,
            })),
            Some("remove")
        );
        assert_eq!(
            remote_refused_command(&Command::Login(LoginArgs {
                api: false,
                codex: false,
                grok: false,
            })),
            Some("login")
        );
        assert_eq!(
            remote_refused_command(&Command::Import(ImportArgs {
                from: None,
                json: None,
            })),
            Some("import")
        );
    }

    #[test]
    fn remote_refused_command_allows_targeting_and_local_commands() {
        // Commands that TARGET the remote (status/accounts/env) or stay LOCAL by
        // design (channel/update) must NOT be refused.
        assert_eq!(
            remote_refused_command(&Command::Status(StatusArgs { json: false })),
            None
        );
        assert_eq!(
            remote_refused_command(&Command::Accounts(AccountsArgs {
                verbose: false,
                json: false,
            })),
            None
        );
        assert_eq!(remote_refused_command(&Command::Env(EnvArgs {})), None);
        assert_eq!(
            remote_refused_command(&Command::Channel(ChannelArgs { channel: None })),
            None
        );
        assert_eq!(
            remote_refused_command(&Command::Update(UpdateArgs {})),
            None
        );
    }

    #[test]
    fn refuse_remote_names_the_remote_and_the_escape_hatch() {
        let endpoint = Endpoint {
            base_url: "http://llmux-host:3456".into(),
            api_key: None,
            remote: true,
            host: "llmux-host".into(),
            port: 3456,
        };
        let CliError::Message(msg) = refuse_remote("stop", &endpoint) else {
            panic!("expected a Message error");
        };
        // Names the command, the remote target, and how to act locally instead.
        assert!(msg.contains("llmux stop"), "{msg}");
        assert!(msg.contains("llmux-host:3456"), "{msg}");
        assert!(msg.contains("--remote"), "{msg}");
        assert!(msg.contains("remote.host"), "{msg}");
    }
}
