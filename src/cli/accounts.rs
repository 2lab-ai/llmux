//! `llmux accounts [-v|--json]` and `llmux remove <name>` — account
//! roster management. The default listing and `remove` work purely from the
//! config file (no network, no running server required); `--json` is the one
//! exception — it pulls the live usage dashboard from the running server.

use crate::config::{AccountCredential, Config};

use super::daemon::{self, ServerProbe};
use super::{now_ms, prompt_line, resolve_endpoint, AccountsArgs, CliError, Endpoint, RemoveArgs};

/// List configured accounts: name, type, tier when stored, masked
/// credential; `-v` adds token expiry detail. `--json` instead emits the live
/// dashboard document (see [`list_live`]).
///
/// The offline table reads THIS machine's config, so it only makes sense for
/// the LOCAL daemon. `--json` — and ANY remote invocation (`--remote` /
/// `remote.host`) — instead reads the live account pool from the resolved
/// daemon, so a remote client sees the remote's shared pool (the wrong pool
/// would be this machine's empty config) rather than silently listing nothing.
pub async fn list(args: AccountsArgs, remote: Option<String>) -> Result<(), CliError> {
    let config = crate::config::load_or_init()?;
    let endpoint = resolve_endpoint(remote.as_deref(), &config)?;

    if args.json || endpoint.remote {
        return list_live(&endpoint).await;
    }

    if config.accounts.is_empty() {
        println!("No accounts configured.");
        println!("Add one with: llmux import, llmux login, or llmux login --api");
        return Ok(());
    }

    for (i, account) in config.accounts.iter().enumerate() {
        match &account.credential {
            AccountCredential::Apikey { api_key } => {
                println!("  [{}] {} (apikey)  {}", i + 1, account.name, mask(api_key));
            }
            AccountCredential::OpenRouter { api_key, label } => {
                println!(
                    "  [{}] {} (openrouter)  {}",
                    i + 1,
                    account.name,
                    mask(api_key)
                );
                if args.verbose && !label.is_empty() {
                    println!("       Key label: {label}");
                }
            }
            AccountCredential::Oauth {
                account_uuid,
                expires_at_ms,
                tier,
                ..
            } => {
                let tier_label = tier
                    .as_deref()
                    .map(|t| format!(", {t}"))
                    .unwrap_or_default();
                println!("  [{}] {} (oauth{tier_label})", i + 1, account.name);
                if args.verbose {
                    if !account_uuid.is_empty() {
                        println!("       Uuid:  {account_uuid}");
                    }
                    println!(
                        "       Token: {}",
                        describe_expiry(*expires_at_ms, now_ms())
                    );
                }
            }
            AccountCredential::Codex {
                account_id,
                expires_at_ms,
                ..
            } => {
                println!("  [{}] {} (codex)", i + 1, account.name);
                if args.verbose {
                    if !account_id.is_empty() {
                        println!("       Account: {account_id}");
                    }
                    println!(
                        "       Token: {}",
                        describe_expiry(*expires_at_ms, now_ms())
                    );
                }
            }
            AccountCredential::Grok {
                subject,
                expires_at_ms,
                ..
            } => {
                println!("  [{}] {} (grok)", i + 1, account.name);
                if args.verbose {
                    if !subject.is_empty() {
                        println!("       Subject: {subject}");
                    }
                    println!(
                        "       Token: {}",
                        describe_expiry(*expires_at_ms, now_ms())
                    );
                }
            }
        }
    }
    Ok(())
}

/// Print the live account dashboard document from a resolved daemon endpoint
/// (local or remote) as JSON — used by `llmux accounts --json` and by any
/// remote `llmux accounts` (a pure client has no local pool to list offline).
///
/// The usage windows the user wants (5h/7d utilization + resets, in-flight,
/// token health) live only in the running server, so this mirrors
/// `llmux status --json`'s probe + exit-code contract (0 = server running,
/// 1 = not running) rather than the offline config path. The body is the
/// `/llmux/status` document — the account-centric slice the dashboard is built
/// from: top-level `current` / `current_by_group` (the selected subscription,
/// per backend group) plus a per-account array carrying `group`, `status`,
/// `order`, `five_hour` / `seven_day` (`utilization` + `resets_at` /
/// `resets_in_secs`), `in_flight`, and `token_expires_at_ms` /
/// `last_refresh_ms`. Pretty-printed (`{:#}`) so it is readable as well as
/// machine-parseable.
async fn list_live(endpoint: &Endpoint) -> Result<(), CliError> {
    let port = endpoint.port;

    match daemon::probe_server(&endpoint.base_url, endpoint.api_key.as_deref()).await? {
        ServerProbe::Running { status } => {
            println!("{status:#}");
            Ok(())
        }
        ServerProbe::NotRunning => {
            println!(
                "{:#}",
                serde_json::json!({ "server": "not running", "port": port })
            );
            std::process::exit(1);
        }
        ServerProbe::Unauthorized => Err(CliError::Message(format!(
            "llmux on port {port} rejected the api key (401) — check `remote.api_key` \
             (remote) or `proxy.api_key` (local) in the config"
        ))),
        ServerProbe::Foreign { detail } => Err(CliError::Message(format!(
            "port {port} answers but is not llmux: {detail}"
        ))),
    }
}

/// Remove one account by name via read-merge-write (`config::update`) so a
/// concurrently running server's writes are not clobbered. Asks for
/// confirmation unless `--yes` (non-TTY stdin requires `--yes`).
pub async fn remove(args: RemoveArgs) -> Result<(), CliError> {
    use std::io::IsTerminal as _;

    // Existence pre-check for a friendly error (re-checked inside update).
    let config = crate::config::load()?;
    if !config.accounts.iter().any(|a| a.name == args.name) {
        return Err(CliError::Message(format!(
            "account {:?} not found (see `llmux accounts`)",
            args.name
        )));
    }

    if !args.yes {
        if !std::io::stdin().is_terminal() {
            return Err(CliError::Message(format!(
                "refusing to remove {:?} without confirmation; pass --yes",
                args.name
            )));
        }
        let answer = prompt_line(&format!("Remove account {:?}? [y/N] ", args.name))?;
        if !matches!(answer.to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }

    let mut removed = false;
    crate::config::update(|c: &mut Config| {
        removed = c.remove_account(&args.name);
    })?;

    if removed {
        println!("Removed account {:?}", args.name);
        Ok(())
    } else {
        // Lost a race with another writer that removed it first.
        Err(CliError::Message(format!(
            "account {:?} was already removed",
            args.name
        )))
    }
}

/// Show a credential prefix only — enough to recognize, useless to leak.
pub(crate) fn mask(secret: &str) -> String {
    let prefix: String = secret.chars().take(15).collect();
    if secret.chars().count() > 15 {
        format!("{prefix}...")
    } else {
        prefix
    }
}

fn describe_expiry(expires_at_ms: u64, now_ms: u64) -> String {
    if expires_at_ms == 0 {
        return "expiry unknown".to_string();
    }
    if expires_at_ms <= now_ms {
        return "expired".to_string();
    }
    let mins = (expires_at_ms - now_ms) / 60_000;
    let (hours, mins) = (mins / 60, mins % 60);
    if hours > 0 {
        format!("expires in {hours}h {mins}m")
    } else {
        format!("expires in {mins}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_keeps_prefix_only() {
        assert_eq!(mask("sk-ant-api03-SECRETSECRET"), "sk-ant-api03-SE...");
        assert_eq!(mask("short"), "short");
    }

    #[test]
    fn expiry_descriptions() {
        let now = 1_000_000_000_000;
        assert_eq!(describe_expiry(0, now), "expiry unknown");
        assert_eq!(describe_expiry(now - 1, now), "expired");
        assert_eq!(describe_expiry(now + 5 * 60_000, now), "expires in 5m");
        assert_eq!(describe_expiry(now + 90 * 60_000, now), "expires in 1h 30m");
    }

    /// The live listing follows the endpoint it is given, NOT the local config:
    /// point `list_live` at a mock llmux `/llmux/status` server (as a remote
    /// endpoint would be resolved) and it probes THAT address and returns Ok —
    /// proving `accounts` reads the resolved (remote) pool, not this machine's.
    #[tokio::test]
    async fn list_live_follows_the_given_endpoint() {
        use axum::routing::get;
        use axum::Router;

        let body = serde_json::json!({
            "version": crate::build_info::version_string(),
            "current": null,
            "accounts": [],
        })
        .to_string();
        let app = Router::new().route("/llmux/status", get(move || async move { body }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let endpoint = Endpoint {
            base_url: format!("http://127.0.0.1:{port}"),
            api_key: Some("lm-remote".into()),
            remote: true,
            host: "127.0.0.1".into(),
            port,
        };
        // Running (llmux-shaped 2xx) → Ok; this only passes if the probe hit the
        // endpoint's own base_url rather than the local proxy port.
        list_live(&endpoint).await.unwrap();
    }
}
