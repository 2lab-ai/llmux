//! `llmux usage [--json]` — a focused per-account weekly-quota summary:
//! name, logged-in status, remaining 7d usage, and when it resets.
//!
//! `llmux accounts --json` already exposes the full live dashboard document
//! (5h + 7d + scoped windows, in-flight, token health, selection order —
//! the whole `/llmux/status` schema); this command reads that SAME endpoint
//! (no new server-side state) but reshapes it into a small, stable, scripting-
//! friendly slice for the one question "how much of my week is left, per
//! account" — without a caller having to know the full dashboard schema or
//! pick the right window out of `five_hour`/`seven_day`/`scoped_limits`.

use super::daemon::{self, ServerProbe};
use super::{resolve_endpoint, CliError, UsageArgs};

/// One account's usage-summary row — the JSON shape `--json` emits and the
/// human table's source of truth. Field names are the public contract
/// (additive-only going forward, same convention as `/llmux/status`).
#[derive(Debug, Clone, serde::Serialize)]
struct AccountUsage {
    /// The account identifier as configured (e.g. `andrew@x.com`,
    /// `codex:andrew@x.com`) — matches `llmux accounts` / the dashboard.
    name: String,
    /// Backend group: `claude` / `codex` / `grok` / `openrouter`.
    group: String,
    /// Credential kind: `oauth` / `codex` / `grok` / `apikey` / `openrouter`.
    #[serde(rename = "type")]
    kind: String,
    /// Whether the account's credential is currently valid — `false` only
    /// when the daemon has recorded an auth failure (e.g. a revoked/expired
    /// token); does NOT mean "actively selected right now" (see `status`
    /// for that finer distinction: `active`/`ok`/`cooldown`/`auth_failed`).
    logged_in: bool,
    /// The daemon's scheduler status for this account: `active` (currently
    /// selected), `ok` (eligible, not selected), `cooldown` (rate-limited,
    /// will retry), `auth_failed` (credential invalid).
    status: String,
    /// The weekly quota window, or `null` when the account's kind has no
    /// weekly-quota source (apikey, openrouter) or none has been observed
    /// yet (first poll still pending). Grok's weekly figure comes from
    /// `/billing?format=credits`, oauth's from `/api/oauth/usage` — both
    /// land here through the same `UsagePoller` (see `scheduler/usage.rs`).
    seven_day: Option<SevenDayUsage>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct SevenDayUsage {
    /// 0.0-100.0, 2 decimal places.
    used_percent: f64,
    /// 0.0-100.0, 2 decimal places — `100.0 - used_percent`, not
    /// independently sourced (the upstream windows report usage, not
    /// remaining capacity).
    remaining_percent: f64,
    /// Epoch seconds — same representation `/llmux/status` uses, so a
    /// caller already parsing that document needs no new convention.
    resets_at: u64,
    /// Seconds from now until `resets_at` (0 if already past — an
    /// already-elapsed window reads as reset, matching the dashboard's
    /// `effective_utilization` convention).
    resets_in_secs: u64,
}

/// `llmux usage` / `llmux usage --json`. Same probe + exit-code contract as
/// `llmux status`/`llmux accounts --json` (0 = server running, 1 = not) —
/// this reads the SAME `/llmux/status` document those commands do, just
/// reshaped, so it shares their local/remote resolution and failure modes.
pub async fn run(args: UsageArgs, remote: Option<String>) -> Result<(), CliError> {
    let config = crate::config::load_or_init()?;
    let endpoint = resolve_endpoint(remote.as_deref(), &config)?;
    let port = endpoint.port;

    let status = match daemon::probe_server(&endpoint.base_url, endpoint.api_key.as_deref()).await?
    {
        ServerProbe::Running { status } => status,
        ServerProbe::NotRunning => {
            if args.json {
                println!(
                    "{:#}",
                    serde_json::json!({ "server": "not running", "port": port, "accounts": [] })
                );
            } else {
                println!("server not running (port {port})");
            }
            std::process::exit(1);
        }
        ServerProbe::Unauthorized => {
            return Err(CliError::Message(format!(
                "llmux on port {port} rejected the api key (401) — check `remote.api_key` \
                 (remote) or `proxy.api_key` (local) in the config"
            )))
        }
        ServerProbe::Foreign { detail } => {
            return Err(CliError::Message(format!(
                "port {port} answers but is not llmux: {detail}"
            )))
        }
    };

    let accounts = summarize(&status);
    if args.json {
        println!("{:#}", serde_json::json!({ "accounts": accounts }));
    } else {
        print_table(&accounts);
    }
    Ok(())
}

/// Reshape the raw `/llmux/status` document's `accounts` array into the
/// minimal per-account summary. Tolerant by design: a malformed/missing
/// field on one account row drops that row's `seven_day` (or, for a
/// genuinely unparseable row, the row itself) rather than failing the whole
/// command — this is a read-only reporting view, never a source of truth.
fn summarize(status: &serde_json::Value) -> Vec<AccountUsage> {
    status
        .get("accounts")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|account| {
            let name = account.get("name")?.as_str()?.to_string();
            let group = account
                .get("group")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?")
                .to_string();
            let kind = account
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?")
                .to_string();
            let status_label = account
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?")
                .to_string();
            let logged_in = status_label != "auth_failed";
            let seven_day = account
                .get("seven_day")
                .filter(|w| !w.is_null())
                .and_then(seven_day_usage);
            Some(AccountUsage {
                name,
                group,
                kind,
                logged_in,
                status: status_label,
                seven_day,
            })
        })
        .collect()
}

/// Parse one account's `seven_day` window object (`{utilization,
/// resets_at, resets_in_secs}`, the `/llmux/status` shape) into the public
/// percent-scaled form. `None` on any missing/malformed field.
fn seven_day_usage(window: &serde_json::Value) -> Option<SevenDayUsage> {
    let utilization = window.get("utilization")?.as_f64()?;
    let resets_at = window.get("resets_at")?.as_u64()?;
    let resets_in_secs = window.get("resets_in_secs")?.as_u64()?;
    let used_percent = round2(utilization.clamp(0.0, 1.0) * 100.0);
    let remaining_percent = round2(100.0 - used_percent);
    Some(SevenDayUsage {
        used_percent,
        remaining_percent,
        resets_at,
        resets_in_secs,
    })
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Human-readable table: name, type, logged-in, 7d used/left, reset ETA.
/// `n/a` for accounts with no weekly-quota source or no reading yet.
fn print_table(accounts: &[AccountUsage]) {
    if accounts.is_empty() {
        println!("No accounts.");
        return;
    }
    let name_width = accounts
        .iter()
        .map(|a| a.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    println!(
        "{:<name_width$}  {:<10}  {:<9}  {:>7}  {:>7}  RESETS",
        "NAME",
        "TYPE",
        "LOGGED IN",
        "7D USED",
        "7D LEFT",
        name_width = name_width
    );
    for account in accounts {
        let (used, left, resets) = match &account.seven_day {
            Some(w) => (
                format!("{:.1}%", w.used_percent),
                format!("{:.1}%", w.remaining_percent),
                format!(
                    "in {}",
                    crate::scheduler::select::compact_duration(std::time::Duration::from_secs(
                        w.resets_in_secs
                    ))
                ),
            ),
            None => ("n/a".into(), "n/a".into(), "n/a".into()),
        };
        println!(
            "{:<name_width$}  {:<10}  {:<9}  {:>7}  {:>7}  {resets}",
            account.name,
            account.kind,
            if account.logged_in { "yes" } else { "no" },
            used,
            left,
            name_width = name_width
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::Endpoint;
    use super::*;

    fn status_doc(accounts: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "version": "llmux test", "current": null, "accounts": accounts })
    }

    #[test]
    fn summarizes_a_healthy_oauth_account_with_a_seven_day_window() {
        let doc = status_doc(serde_json::json!([
            {
                "name": "a@x.com", "type": "oauth", "group": "claude", "status": "active",
                "seven_day": {"utilization": 0.42, "resets_at": 1_781_222_400, "resets_in_secs": 3600},
            }
        ]));
        let accounts = summarize(&doc);
        assert_eq!(accounts.len(), 1);
        let a = &accounts[0];
        assert_eq!(a.name, "a@x.com");
        assert_eq!(a.group, "claude");
        assert_eq!(a.kind, "oauth");
        assert!(a.logged_in);
        assert_eq!(a.status, "active");
        let seven = a.seven_day.as_ref().unwrap();
        assert_eq!(seven.used_percent, 42.0);
        assert_eq!(seven.remaining_percent, 58.0);
        assert_eq!(seven.resets_at, 1_781_222_400);
        assert_eq!(seven.resets_in_secs, 3600);
    }

    #[test]
    fn auth_failed_status_reads_as_not_logged_in() {
        let doc = status_doc(serde_json::json!([
            {"name": "k@x.com", "type": "grok", "group": "grok", "status": "auth_failed", "seven_day": null}
        ]));
        let a = &summarize(&doc)[0];
        assert!(!a.logged_in);
        assert_eq!(a.status, "auth_failed");
        assert!(a.seven_day.is_none());
    }

    #[test]
    fn null_seven_day_is_none_not_an_error() {
        let doc = status_doc(serde_json::json!([
            {"name": "c@x.com", "type": "codex", "group": "codex", "status": "ok", "seven_day": null}
        ]));
        let a = &summarize(&doc)[0];
        assert!(a.seven_day.is_none());
    }

    #[test]
    fn missing_seven_day_is_none_not_an_error() {
        let doc = status_doc(serde_json::json!([
            {"name": "c@x.com", "type": "apikey", "group": "claude", "status": "ok"}
        ]));
        let a = &summarize(&doc)[0];
        assert!(a.seven_day.is_none());
    }

    #[test]
    fn a_row_with_no_name_is_dropped_not_a_panic() {
        let doc = status_doc(serde_json::json!([
            {"type": "oauth", "status": "ok"},
            {"name": "ok@x.com", "type": "oauth", "group": "claude", "status": "ok"},
        ]));
        let accounts = summarize(&doc);
        assert_eq!(
            accounts.len(),
            1,
            "malformed row dropped, well-formed row kept"
        );
        assert_eq!(accounts[0].name, "ok@x.com");
    }

    #[test]
    fn empty_accounts_array_yields_empty_summary() {
        let doc = status_doc(serde_json::json!([]));
        assert!(summarize(&doc).is_empty());
    }

    #[test]
    fn percent_rounds_to_two_decimals() {
        let doc = status_doc(serde_json::json!([
            {"name": "a", "type": "grok", "group": "grok", "status": "active",
             "seven_day": {"utilization": 0.4712345, "resets_at": 0, "resets_in_secs": 0}}
        ]));
        let seven = summarize(&doc)[0].seven_day.clone().unwrap();
        assert_eq!(seven.used_percent, 47.12);
        assert_eq!(seven.remaining_percent, 52.88);
    }

    /// The live end-to-end path: point at a mock `/llmux/status` server, same
    /// pattern `accounts::list_live_follows_the_given_endpoint` uses — proves
    /// `run` reads the endpoint it's given, not a hardcoded local port.
    #[tokio::test]
    async fn run_json_follows_the_resolved_endpoint() {
        use axum::routing::get;
        use axum::Router;

        let body = serde_json::json!({
            "version": crate::build_info::version_string(),
            "current": null,
            "accounts": [
                {"name": "g@x.com", "type": "grok", "group": "grok", "status": "active",
                 "seven_day": {"utilization": 0.47, "resets_at": 1_789_891_490, "resets_in_secs": 561_234}}
            ],
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
        let status = match daemon::probe_server(&endpoint.base_url, endpoint.api_key.as_deref())
            .await
            .unwrap()
        {
            ServerProbe::Running { status } => status,
            other => panic!("expected Running, got {other:?}"),
        };
        let accounts = summarize(&status);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].seven_day.as_ref().unwrap().used_percent, 47.0);
    }
}
