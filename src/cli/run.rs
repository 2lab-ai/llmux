//! `llmux run [-- args]` — ensure the proxy is running (auto-starting a
//! background daemon when needed), then spawn `claude` with the proxy env
//! injected.

use super::daemon::{ensure_server_running, EnsureOutcome};
use super::{resolve_endpoint, CliError, Endpoint, RunArgs};

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

    let (base_url, api_key, remove_key) = claude_env(&endpoint);
    let mut command = tokio::process::Command::new("claude");
    command
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
}
