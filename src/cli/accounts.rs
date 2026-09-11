//! `llmux accounts [-v|--json]`, its usage-control subcommands
//! (`refresh`/`resets`/`reset`, .prd/16) and `llmux remove <name>` — account
//! roster management. The default listing and `remove` work purely from the
//! config file (no network, no running server required); `--json` is the one
//! exception — it pulls the live usage dashboard from the running server.
//!
//! The usage controls all act on the TARGET DAEMON (local or `--remote`): the
//! daemon owns the credentials and is the only place an upstream read/redeem
//! may happen. This file is a thin, honest client: it never invents a count
//! (unknown is printed as `unknown`, never 0), never mints a second
//! idempotency key for a retry, and never reports a non-success outcome as a
//! reset.

use crate::auth::codex_usage::ResetCredit;
use crate::config::{AccountCredential, Config};
use crate::proxy::usage_controls::{
    ConsumeResponse, RefreshResponse, RefreshResult, ResetCreditsResponse, ResetOutcome,
};

use super::daemon::{self, ServerProbe};
use super::{
    now_ms, prompt_line, resolve_endpoint, AccountsArgs, AccountsCommand, AccountsResetArgs,
    CliError, Endpoint, RemoveArgs,
};

/// Timeouts for the control calls. A redemption POST may wait on an upstream
/// round-trip, so it gets more room than a plain read — but it is still
/// BOUNDED: an unbounded wait would leave the operator unable to tell a slow
/// success from a hang, and the safe recovery is an explicit same-id retry.
const CONTROL_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const CONTROL_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// `llmux accounts [SUBCOMMAND]` — route the bare listing (offline roster /
/// live dashboard) or one of the usage-control subcommands. The endpoint is
/// resolved ONCE here for the subcommands so local and `--remote` share the
/// same code path and the same `x-api-key`.
pub async fn run(args: AccountsArgs, remote: Option<String>) -> Result<(), CliError> {
    let AccountsArgs {
        verbose,
        json,
        command,
    } = args;
    let Some(command) = command else {
        return list(
            AccountsArgs {
                verbose,
                json,
                command: None,
            },
            remote,
        )
        .await;
    };
    let config = crate::config::load_or_init()?;
    let endpoint = resolve_endpoint(remote.as_deref(), &config)?;
    match command {
        AccountsCommand::Refresh(args) => refresh(&endpoint, args.account.as_deref()).await,
        AccountsCommand::Resets(args) => resets(&endpoint, &args.account).await,
        AccountsCommand::Reset(args) => reset(&endpoint, args).await,
    }
}

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

// ---------------------------------------------------------------------------
// Usage controls (.prd/16): refresh · resets · reset
// ---------------------------------------------------------------------------

/// A bounded HTTP client for the control calls.
fn control_client() -> Result<reqwest::Client, CliError> {
    reqwest::Client::builder()
        .connect_timeout(CONTROL_CONNECT_TIMEOUT)
        .timeout(CONTROL_READ_TIMEOUT)
        .build()
        .map_err(|err| CliError::Message(format!("http client init failed: {err}")))
}

/// Error text for a non-2xx control response: prefer the daemon's own
/// `error.message`, fall back to the status code. Never echoes credentials —
/// only the JSON message field the daemon chose to expose.
fn control_error(status: reqwest::StatusCode, body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| status.to_string())
}

/// `unknown` (never observed) vs `N owned` vs `N owned · M applicable now` —
/// the three states the operator must be able to tell apart. An absent count
/// is NOT zero: llmux has not looked, so it does not claim.
fn reset_count_label(available: Option<u64>, applicable: Option<u64>) -> String {
    match (available, applicable) {
        (None, _) => "unknown".to_string(),
        (Some(0), _) => "0 owned".to_string(),
        (Some(n), None) => format!("{n} owned · applicable unknown"),
        (Some(n), Some(m)) => format!("{n} owned · {m} applicable now"),
    }
}

/// Per-account report lines + how many accounts FAILED. A partial failure is
/// visible per account and fails the command — the caller never sums it into a
/// green.
fn refresh_report(results: &[RefreshResult]) -> (Vec<String>, usize) {
    let mut failed = 0;
    let lines = results
        .iter()
        .map(|r| {
            if r.ok {
                let control = r.usage_control.as_ref();
                format!(
                    "  {} — refreshed · resets {}",
                    r.account,
                    reset_count_label(
                        control.and_then(|c| c.available_resets),
                        control.and_then(|c| c.applicable_resets),
                    )
                )
            } else {
                failed += 1;
                format!(
                    "  {} — FAILED: {}",
                    r.account,
                    r.error.as_deref().unwrap_or("unspecified error")
                )
            }
        })
        .collect();
    (lines, failed)
}

/// `llmux accounts refresh [ACCOUNT]` — ask the daemon to re-read usage from
/// the provider NOW. Omitting the account refreshes every supported account
/// (the key is OMITTED from the body, never sent as null).
async fn refresh(endpoint: &Endpoint, account: Option<&str>) -> Result<(), CliError> {
    let url = format!("{}/llmux/refresh-usage", endpoint.base_url);
    let body = match account {
        Some(name) => serde_json::json!({ "account": name }),
        None => serde_json::json!({}),
    };
    let mut request = control_client()?.post(&url).json(&body);
    if let Some(key) = endpoint.api_key.as_deref() {
        request = request.header("x-api-key", key);
    }
    let response = request
        .send()
        .await
        .map_err(|err| CliError::Message(format!("refresh request failed: {err}")))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(CliError::Message(format!(
            "refresh failed: {}",
            control_error(status, &text)
        )));
    }
    let ack: RefreshResponse = serde_json::from_str(&text)
        .map_err(|err| CliError::Message(format!("refresh response parse failed: {err}")))?;
    if ack.results.is_empty() {
        return Err(CliError::Message(
            "refresh returned no accounts — nothing was refreshed".into(),
        ));
    }
    let (lines, failed) = refresh_report(&ack.results);
    for line in &lines {
        println!("{line}");
    }
    if failed > 0 {
        // Name the failures in the error too: the exit code says "something
        // failed", the message must say WHICH account.
        let names: Vec<&str> = ack
            .results
            .iter()
            .filter(|r| !r.ok)
            .map(|r| r.account.as_str())
            .collect();
        return Err(CliError::Message(format!(
            "{failed} of {} account(s) failed to refresh: {}",
            ack.results.len(),
            names.join(", ")
        )));
    }
    Ok(())
}

/// Percent-encode one query-string VALUE (RFC 3986 unreserved set kept).
/// Account names are emails (`@`, `.`, `+`) and group-prefixed ids
/// (`codex:me@x.com`), so the `account=` parameter must be escaped rather than
/// interpolated raw.
fn query_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Fetch the account's reset entitlements (read-only).
async fn fetch_resets(
    endpoint: &Endpoint,
    account: &str,
) -> Result<ResetCreditsResponse, CliError> {
    let url = format!(
        "{}/llmux/reset-credits?account={}",
        endpoint.base_url,
        query_escape(account)
    );
    let mut request = control_client()?.get(&url);
    if let Some(key) = endpoint.api_key.as_deref() {
        request = request.header("x-api-key", key);
    }
    let response = request
        .send()
        .await
        .map_err(|err| CliError::Message(format!("reset-credits request failed: {err}")))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(CliError::Message(format!(
            "reset-credits failed: {}",
            control_error(status, &text)
        )));
    }
    serde_json::from_str(&text)
        .map_err(|err| CliError::Message(format!("reset-credits parse failed: {err}")))
}

/// `llmux accounts resets ACCOUNT` — print the entitlement list. Read-only:
/// this command can never redeem.
async fn resets(endpoint: &Endpoint, account: &str) -> Result<(), CliError> {
    let list = fetch_resets(endpoint, account).await?;
    println!(
        "{account} — resets {}",
        reset_count_label(list.available_count, list.applicable_available_count)
    );
    if let Some(warning) = &list.applicability_warning {
        println!("  note: {warning}");
    }
    if let Some(pending) = &list.pending_request_id {
        println!(
            "  pending redemption: request id {pending} — resolve it with \
             `llmux accounts reset {account} --request-id {pending} --yes`"
        );
    }
    if list.credits.is_empty() {
        println!("  (no credit detail reported)");
        return Ok(());
    }
    for credit in &list.credits {
        let title = credit.title.as_deref().unwrap_or("reset");
        let status = credit.status.as_deref().unwrap_or("unknown status");
        let kind = credit.reset_type.as_deref().unwrap_or("unknown type");
        let id = credit
            .id
            .as_deref()
            .map(|id| format!(" · id {id}"))
            .unwrap_or_default();
        println!("  [{status}] {title} ({kind}){id}");
        if let Some(granted) = &credit.granted_at {
            let expires = credit
                .expires_at
                .as_deref()
                .map(|e| format!(" · expires {e}"))
                .unwrap_or_default();
            println!("       granted {granted}{expires}");
        }
        if let Some(description) = &credit.description {
            println!("       {description}");
        }
    }
    Ok(())
}

/// Pick the credit a NEW redemption would spend, or say why none may be spent.
/// Refuses on UNKNOWN inventory and on zero owned — llmux never starts a
/// redemption it has no fresh evidence for. `applicable_available_count == 0`
/// does NOT refuse: its semantics are undocumented and the consume response is
/// the authority.
fn choose_credit<'a>(
    list: &'a ResetCreditsResponse,
    wanted: Option<&str>,
) -> Result<&'a ResetCredit, String> {
    let Some(owned) = list.available_count else {
        return Err(
            "no redeemable reset: the inventory is UNKNOWN — refresh the account first \
             (`llmux accounts refresh ACCOUNT`); llmux will not start a redemption it cannot see"
                .to_string(),
        );
    };
    if owned == 0 {
        return Err("no redeemable reset: the account owns 0 resets".to_string());
    }
    // The daemon computes the same gate from the same list; disagreeing with
    // it is a reason to stop, never to proceed.
    if !list.redeemable {
        return Err(format!(
            "no redeemable reset: {owned} owned, but the daemon reports none redeemable"
        ));
    }
    // An explicitly named credit is the one the operator will be asked to
    // confirm AND the one that gets spent — picking "the first redeemable row"
    // here would describe credit A in the prompt and send credit B.
    if let Some(wanted) = wanted {
        let Some(credit) = list
            .credits
            .iter()
            .find(|c| c.id.as_deref() == Some(wanted))
        else {
            return Err(format!(
                "no credit with id {wanted:?} in this account's inventory (see \
                 `llmux accounts resets ACCOUNT`)"
            ));
        };
        if !credit.is_redeemable() {
            return Err(format!(
                "credit {wanted:?} is not redeemable (status {}, type {})",
                credit.status.as_deref().unwrap_or("unknown"),
                credit.reset_type.as_deref().unwrap_or("unknown"),
            ));
        }
        return Ok(credit);
    }
    list.credits
        .iter()
        .find(|c| c.is_redeemable())
        .ok_or_else(|| {
            format!(
                "no redeemable reset: {owned} owned, but no credit row is `available` with \
                 reset_type `{}`",
                crate::auth::codex_usage::CODEX_RATE_LIMITS
            )
        })
}

/// The confirmation line for a redemption: it names the ACCOUNT and the ONE
/// reset being spent, plus the owned/applicable split, so the operator is
/// never asked a bare y/N with no subject.
fn reset_confirm_prompt(
    account: &str,
    available: Option<u64>,
    applicable: Option<u64>,
    title: Option<&str>,
) -> String {
    format!(
        "Redeem ONE rate-limit reset ({}) for account {account:?}? \
         Inventory: {}. This spends a reset upstream and cannot be undone. [y/N] ",
        title.unwrap_or("Full reset"),
        reset_count_label(available, applicable),
    )
}

/// The exact invocation that safely retries THIS redemption: same request id,
/// same credit. Printed on every uncertain failure — a fresh key could spend a
/// second reset.
fn uncertain_retry_hint(account: &str, request_id: &str, credit_id: Option<&str>) -> String {
    let credit = credit_id
        .map(|id| format!(" --credit-id {id}"))
        .unwrap_or_default();
    format!("retry with the SAME id: llmux accounts reset {account}{credit} --request-id {request_id} --yes")
}

/// Turn a typed consume ack into (message, success). The four terminal codes
/// stay distinct: `reset`/`already_redeemed` are successes (the replay spends
/// nothing extra), `nothing_to_reset`/`no_credit` are non-successes that spent
/// nothing. There is no fifth code — an unrecognized upstream reply reaches
/// the client as the daemon's `uncertain` error, handled at the call site.
fn outcome_report(account: &str, ack: &ConsumeResponse) -> (String, bool) {
    let id = &ack.request_id;
    let windows = format!(" · windows reset: {}", ack.windows_reset);
    let applicability = ack
        .applicability_warning
        .as_deref()
        .map(|w| format!("\n  note: {w}"))
        .unwrap_or_default();
    let warning = ack
        .refresh_warning
        .as_deref()
        .map(|w| format!("\n  warning: redemption succeeded but the follow-up read failed: {w}"))
        .unwrap_or_default();
    match ack.outcome {
        ResetOutcome::Reset => (
            format!(
                "reset redeemed for {account} · request id {id}{windows}{applicability}{warning}"
            ),
            true,
        ),
        ResetOutcome::AlreadyRedeemed => (
            format!(
                "already redeemed for {account} · request id {id} — this replay spent \
                 no second reset{windows}{applicability}{warning}"
            ),
            true,
        ),
        ResetOutcome::NothingToReset => (
            format!(
                "nothing to reset for {account} · request id {id} — upstream reported no \
                 limit window to clear; no reset was spent{applicability}"
            ),
            false,
        ),
        ResetOutcome::NoCredit => (
            format!(
                "no reset credit available upstream for {account} · request id {id} — \
                 nothing was spent{applicability}"
            ),
            false,
        ),
    }
}

/// Operator-facing text for a FAILED consume response, by the daemon's stable
/// error code. The two codes that carry an id are the load-bearing ones:
/// `pending_redemption` hands back the id that must be retried instead of
/// starting a second redemption, and `uncertain` hands back THIS attempt's id
/// (the redemption may already have happened — never mint a new key).
fn consume_error_message(
    account: &str,
    status: reqwest::StatusCode,
    body: &str,
    request_id: &str,
    credit_id: Option<&str>,
) -> String {
    let value: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let code = value["error"]["type"].as_str().unwrap_or("");
    let message = value["error"]["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| status.to_string());
    match code {
        "pending_redemption" => {
            let pending = value["pending_request_id"]
                .as_str()
                .unwrap_or(request_id)
                .to_string();
            let credit = value["pending_credit_id"].as_str().or(credit_id);
            format!(
                "{message} — resolve THAT redemption first: {}",
                uncertain_retry_hint(account, &pending, credit)
            )
        }
        "uncertain" => {
            let id = value["request_id"].as_str().unwrap_or(request_id);
            format!(
                "redemption outcome UNCERTAIN ({message}) — it may already have been \
                 redeemed. Do NOT start a new one; {}",
                uncertain_retry_hint(account, id, credit_id)
            )
        }
        // no_credit / unsupported / unknown_account / busy / invalid: the
        // daemon refused before spending anything.
        _ => format!("redemption refused: {message}"),
    }
}

/// A fresh client-side idempotency key. Minted HERE (never by the daemon) so
/// the operator can repeat exactly this redemption after an uncertain failure.
fn mint_request_id() -> String {
    ulid::Ulid::new().to_string()
}

/// `llmux accounts reset ACCOUNT [--credit-id ID] [--request-id ID] [--yes]` —
/// redeem exactly one reset.
///
/// Order of operations is the safety property: confirmation BEFORE any network
/// call, then (for a NEW redemption only) a fresh entitlement read that must
/// show a redeemable credit, then the request id is printed BEFORE the send.
/// An explicit `--request-id` means RETRY: the inventory gate is skipped (the
/// first attempt may already have consumed the credit) and the given key is
/// reused verbatim.
async fn reset(endpoint: &Endpoint, args: AccountsResetArgs) -> Result<(), CliError> {
    use std::io::IsTerminal as _;

    let AccountsResetArgs {
        account,
        credit_id,
        request_id,
        yes,
    } = args;
    let retry = request_id.is_some();

    // Confirmation gate — before the network, so a refused redemption costs
    // nothing and touches nothing.
    if !yes && !std::io::stdin().is_terminal() {
        return Err(CliError::Message(format!(
            "refusing to redeem a rate-limit reset for {account:?} without confirmation; \
             pass --yes (a redemption spends an upstream reset and cannot be undone)"
        )));
    }

    // A NEW redemption needs FRESH evidence that there is something to spend.
    // A retry deliberately skips this: the first attempt may already have
    // consumed the credit, so a now-empty inventory must not block it.
    let mut title: Option<String> = None;
    let mut owned = None;
    let mut applicable = None;
    // The credit the operator is about to confirm. Bound to the POST below so
    // the request spends exactly what the prompt described — never "whatever
    // the daemon picks next".
    let mut credit_id = credit_id;
    if !retry {
        let list = fetch_resets(endpoint, &account).await?;
        owned = list.available_count;
        applicable = list.applicable_available_count;
        // Refuses (before the prompt and before any POST) when an explicitly
        // named credit is unknown or not redeemable.
        let credit = choose_credit(&list, credit_id.as_deref()).map_err(CliError::Message)?;
        title = credit.title.clone();
        // `id` is an OPTIONAL field on an entitlement row (no claim is made
        // here about how often upstream sends it — the local capture was
        // sanitized, so its absence there is not evidence). When an id is
        // present it pins the choice; when it is absent the daemon picks from
        // the same inventory it just reported.
        if credit_id.is_none() {
            credit_id = credit.id.clone();
        }
    }

    if !yes {
        let answer = prompt_line(&reset_confirm_prompt(
            &account,
            owned,
            applicable,
            title.as_deref(),
        ))?;
        if !matches!(answer.to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted — no reset was spent.");
            return Ok(());
        }
    }

    let request_id = request_id.unwrap_or_else(mint_request_id);
    // Printed BEFORE the send: if this process dies mid-flight, the operator
    // still holds the only key that can safely retry.
    println!(
        "request id {request_id} — reuse it to retry safely ({})",
        uncertain_retry_hint(&account, &request_id, credit_id.as_deref())
    );

    let mut body = serde_json::json!({
        "account": account,
        "redeem_request_id": request_id,
        "confirm": true,
    });
    // Omitted, never serialized as null, when the operator named no credit.
    if let Some(id) = &credit_id {
        body["credit_id"] = serde_json::Value::String(id.clone());
    }
    let url = format!("{}/llmux/reset-credits/consume", endpoint.base_url);
    let mut request = control_client()?.post(&url).json(&body);
    if let Some(key) = endpoint.api_key.as_deref() {
        request = request.header("x-api-key", key);
    }
    let response = request.send().await.map_err(|err| {
        CliError::Message(format!(
            "redemption outcome UNKNOWN — the request failed in transit ({err}). \
             It may already have been redeemed; {}",
            uncertain_retry_hint(&account, &request_id, credit_id.as_deref())
        ))
    })?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();

    // Every refusal path is typed by the daemon's error code — including the
    // two that hand back an id (a pending redemption to resolve, or this
    // attempt's own uncertain id). None of them may be retried with a NEW key.
    if !status.is_success() {
        return Err(CliError::Message(consume_error_message(
            &account,
            status,
            &text,
            &request_id,
            credit_id.as_deref(),
        )));
    }
    let ack: ConsumeResponse = match serde_json::from_str(&text) {
        Ok(ack) => ack,
        Err(err) => {
            return Err(CliError::Message(format!(
                "redemption outcome UNKNOWN — unreadable response ({err}) · request id \
                 {request_id}; {}",
                uncertain_retry_hint(&account, &request_id, credit_id.as_deref())
            )))
        }
    };
    let (message, ok) = outcome_report(&account, &ack);
    println!("{message}");
    // R1 receipt-close failure: upstream reached a terminal outcome but the
    // daemon could not release its durable pending receipt. The redemption
    // SUCCEEDED — the only correct follow-up is repeating THIS id, never a new
    // redemption.
    if let Some(pending) = ack
        .usage_control
        .as_ref()
        .and_then(|c| c.pending_request_id.as_deref())
    {
        println!(
            "  note: the daemon still reports this redemption as pending (request id \
             {pending}) — it was NOT re-spent; if you need to close it out, repeat the \
             same id: {}",
            uncertain_retry_hint(&account, pending, credit_id.as_deref())
        );
    }
    if ok {
        Ok(())
    } else {
        Err(CliError::Message(format!(
            "redemption did not reset anything (see the line above) · request id {request_id}"
        )))
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

    // --- codex usage controls (.prd/16) ------------------------------------

    /// Unknown is never zero: a daemon that has not observed this account's
    /// reset inventory must read as `unknown`, and `available_count` (owned)
    /// must stay visibly distinct from `applicable_available_count` (usable
    /// right now) — a live read recorded in the contract showed 3 owned with 0
    /// applicable.
    #[test]
    fn reset_count_label_keeps_unknown_owned_and_applicable_distinct() {
        assert_eq!(reset_count_label(None, None), "unknown");
        assert_eq!(reset_count_label(None, Some(0)), "unknown");
        assert_eq!(
            reset_count_label(Some(3), None),
            "3 owned · applicable unknown"
        );
        assert_eq!(
            reset_count_label(Some(3), Some(0)),
            "3 owned · 0 applicable now"
        );
        assert_eq!(reset_count_label(Some(0), Some(0)), "0 owned");
    }

    /// A NEW redemption needs a FRESH successful list with a redeemable
    /// credit. Unknown inventory and zero owned both refuse BEFORE any POST;
    /// `applicable_available_count == 0` is an informational warning, not an
    /// authoritative disable (its semantics are undocumented — the consume
    /// response stays authoritative).
    #[test]
    fn new_redemption_gate_refuses_unknown_and_zero_but_not_zero_applicable() {
        use crate::auth::codex_usage::CODEX_RATE_LIMITS;
        // Built through serde, not struct literals: these are the daemon's own
        // wire types, and pinning their exact field SET here would break on
        // every additive field the backend adds.
        let credit = |status: &str, kind: &str| {
            serde_json::json!({
                "reset_type": kind, "status": status, "title": "Full reset",
            })
        };
        let list = |available: Option<u64>,
                    applicable: Option<u64>,
                    redeemable: bool,
                    credits: Vec<serde_json::Value>| {
            serde_json::from_value::<ResetCreditsResponse>(serde_json::json!({
                "account": "codex:a@x.com",
                "available_count": available,
                "applicable_available_count": applicable,
                "credits": credits,
                "redeemable": redeemable,
            }))
            .expect("reset credits")
        };
        // Unknown inventory → refuse.
        let unknown = list(
            None,
            None,
            true,
            vec![credit("available", CODEX_RATE_LIMITS)],
        );
        assert!(
            choose_credit(&unknown, None).is_err(),
            "unknown inventory refuses"
        );
        // Zero owned → refuse.
        let empty = list(Some(0), Some(0), false, Vec::new());
        assert!(choose_credit(&empty, None).is_err(), "zero owned refuses");
        // Owned but nothing applicable NOW → still allowed (warning only).
        let live = list(
            Some(3),
            Some(0),
            true,
            vec![
                credit("redeemed", CODEX_RATE_LIMITS),
                credit("available", CODEX_RATE_LIMITS),
            ],
        );
        let chosen = choose_credit(&live, None).expect("a redeemable credit");
        assert_eq!(chosen.status.as_deref(), Some("available"));
        // A wrong reset_type is not redeemable here — even if the daemon's
        // own flag said otherwise, the client refuses to spend it.
        let other = list(
            Some(1),
            Some(1),
            true,
            vec![credit("available", "some_other_product")],
        );
        assert!(
            choose_credit(&other, None).is_err(),
            "foreign reset_type refuses"
        );
    }

    /// An explicit `--credit-id` must select THAT credit for the
    /// confirmation — the operator confirms the credit that will actually be
    /// spent, not whichever row happens to come first — and an id that names
    /// no redeemable credit refuses before any prompt or POST.
    #[test]
    fn explicit_credit_id_selects_that_credit_for_the_confirmation() {
        use crate::auth::codex_usage::CODEX_RATE_LIMITS;
        let list: ResetCreditsResponse = serde_json::from_value(serde_json::json!({
            "account": "codex:a@x.com",
            "available_count": 2,
            "applicable_available_count": 2,
            "redeemable": true,
            "credits": [
                { "id": "c1", "reset_type": CODEX_RATE_LIMITS, "status": "available",
                  "title": "Title A" },
                { "id": "c2", "reset_type": CODEX_RATE_LIMITS, "status": "available",
                  "title": "Title B" },
                { "id": "c3", "reset_type": CODEX_RATE_LIMITS, "status": "redeemed",
                  "title": "Title C" },
            ],
        }))
        .expect("reset credits");

        // No id named → the first redeemable row.
        assert_eq!(
            choose_credit(&list, None)
                .expect("a credit")
                .title
                .as_deref(),
            Some("Title A")
        );
        // `--credit-id c2` → THAT row, so the prompt names Title B.
        assert_eq!(
            choose_credit(&list, Some("c2"))
                .expect("a credit")
                .title
                .as_deref(),
            Some("Title B"),
            "the confirmation must describe the credit that will be spent"
        );
        // An unknown id, and a known-but-not-redeemable id, both refuse.
        let unknown = choose_credit(&list, Some("nope")).expect_err("unknown id");
        assert!(unknown.contains("nope"), "{unknown}");
        let spent = choose_credit(&list, Some("c3")).expect_err("already redeemed");
        assert!(spent.contains("c3"), "{spent}");
        assert!(spent.contains("redeemed"), "{spent}");
    }

    /// The confirmation names the ACCOUNT and exactly ONE reset — never a
    /// bare y/N with no subject.
    #[test]
    fn reset_confirm_prompt_names_account_and_one_reset() {
        let prompt = reset_confirm_prompt("codex:me@x.com", Some(3), Some(0), Some("Full reset"));
        assert!(prompt.contains("codex:me@x.com"), "{prompt}");
        assert!(prompt.contains("ONE"), "{prompt}");
        assert!(prompt.contains("3 owned"), "{prompt}");
        assert!(prompt.contains("0 applicable now"), "{prompt}");
        assert!(prompt.contains("Full reset"), "{prompt}");
    }

    /// Outcomes never masquerade as one another: `reset` and
    /// `already_redeemed` are both successes (the second spends nothing),
    /// `nothing_to_reset` / `no_credit` are distinct non-successes, and an
    /// unknown outcome is UNCERTAIN — it keeps the request id for a safe
    /// explicit retry instead of suggesting a fresh redemption.
    #[test]
    fn consume_outcome_report_separates_the_four_outcomes() {
        let ack = |outcome: ResetOutcome| {
            serde_json::from_value::<ConsumeResponse>(serde_json::json!({
                "outcome": outcome, "request_id": "01RID", "windows_reset": 1,
            }))
            .expect("consume ack")
        };
        let (msg, ok) = outcome_report("acct", &ack(ResetOutcome::Reset));
        assert!(ok, "{msg}");
        assert!(msg.contains("reset redeemed"), "{msg}");
        let (msg, ok) = outcome_report("acct", &ack(ResetOutcome::AlreadyRedeemed));
        assert!(ok, "{msg}");
        assert!(msg.contains("already redeemed"), "{msg}");
        assert!(
            msg.contains("no second reset"),
            "idempotent replay must say nothing more was spent: {msg}"
        );
        let (msg, ok) = outcome_report("acct", &ack(ResetOutcome::NothingToReset));
        assert!(!ok, "{msg}");
        assert!(msg.contains("nothing to reset"), "{msg}");
        let (msg, ok) = outcome_report("acct", &ack(ResetOutcome::NoCredit));
        assert!(!ok, "{msg}");
        assert!(msg.contains("no reset credit"), "{msg}");
    }

    /// The daemon's `uncertain` error keeps THIS attempt's id and points at a
    /// same-key retry; a `pending_redemption` conflict points at the OTHER
    /// id. Neither ever suggests a fresh key.
    #[test]
    fn consume_error_messages_carry_the_retryable_request_id() {
        let uncertain = serde_json::json!({
            "type": "error",
            "error": { "type": "uncertain", "message": "redemption outcome is uncertain (timeout)" },
            "request_id": "01RID",
        })
        .to_string();
        let msg = consume_error_message(
            "acct",
            reqwest::StatusCode::BAD_GATEWAY,
            &uncertain,
            "01RID",
            None,
        );
        assert!(msg.contains("UNCERTAIN"), "{msg}");
        assert!(msg.contains("--request-id 01RID"), "{msg}");

        let pending = serde_json::json!({
            "type": "error",
            "error": { "type": "pending_redemption", "message": "account acct has a pending redemption (request id 01OLD)" },
            "pending_request_id": "01OLD",
        })
        .to_string();
        let msg = consume_error_message(
            "acct",
            reqwest::StatusCode::CONFLICT,
            &pending,
            "01NEW",
            None,
        );
        assert!(msg.contains("--request-id 01OLD"), "{msg}");
        assert!(
            !msg.contains("01NEW"),
            "the new key is never advertised: {msg}"
        );

        // A plain refusal (nothing spent) must NOT read as retryable.
        let refused = serde_json::json!({
            "type": "error",
            "error": { "type": "unsupported", "message": "account a (apikey) has no usage controls" },
        })
        .to_string();
        let msg = consume_error_message(
            "acct",
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            &refused,
            "01RID",
            None,
        );
        assert!(msg.contains("refused"), "{msg}");
        assert!(!msg.contains("--request-id"), "{msg}");
    }

    /// A post-success refresh failure is a SUCCESSFUL redemption with a
    /// stale-read warning — never a retryable redemption failure.
    #[test]
    fn post_success_refresh_failure_stays_a_success_with_warning() {
        let ack: ConsumeResponse = serde_json::from_value(serde_json::json!({
            "outcome": "reset", "request_id": "01RID", "windows_reset": 1,
            "refresh_warning": "usage re-read failed: upstream 502",
        }))
        .expect("consume ack");
        let (msg, ok) = outcome_report("acct", &ack);
        assert!(ok, "redemption still succeeded: {msg}");
        assert!(msg.contains("warning"), "{msg}");
        assert!(msg.contains("upstream 502"), "{msg}");
        assert!(
            !msg.contains("--request-id"),
            "a succeeded redemption must not advertise a retry: {msg}"
        );
    }

    /// An uncertain transport failure prints the SAME request id and the exact
    /// retry invocation — never a new key.
    #[test]
    fn uncertain_send_failure_retains_the_request_id_in_the_retry_hint() {
        let hint = uncertain_retry_hint("acct", "01RID", Some("cred-7"));
        assert!(hint.contains("01RID"), "{hint}");
        assert!(hint.contains("--request-id 01RID"), "{hint}");
        assert!(hint.contains("--credit-id cred-7"), "{hint}");
        assert!(hint.contains("acct"), "{hint}");
        let plain = uncertain_retry_hint("acct", "01RID", None);
        assert!(!plain.contains("--credit-id"), "{plain}");
    }

    /// `refresh` reports per-account results and a partial failure is NOT
    /// silently green: the failing account keeps its error and the command
    /// fails.
    #[test]
    fn refresh_report_surfaces_partial_failures() {
        // Built through serde, not a struct literal: this is the daemon's own
        // wire type, and a test that pins its exact field SET would break on
        // every additive field the backend adds (and teach nothing).
        let results: Vec<RefreshResult> = serde_json::from_value(serde_json::json!([
            { "account": "ok-acct", "ok": true, "provider": "codex",
              "usage_control": { "available_resets": 3, "applicable_resets": 0 } },
            { "account": "bad-acct", "ok": false, "provider": "codex",
              "error": "upstream 502" },
        ]))
        .expect("refresh results");
        let (lines, failed) = refresh_report(&results);
        assert_eq!(failed, 1);
        assert!(lines[0].contains("ok-acct"), "{:?}", lines);
        assert!(lines[0].contains("3 owned"), "{:?}", lines);
        assert!(lines[1].contains("bad-acct"), "{:?}", lines);
        assert!(lines[1].contains("upstream 502"), "{:?}", lines);
        // A response with no rows at all is not a green either.
        let (_, failed) = refresh_report(&[]);
        assert_eq!(
            failed, 0,
            "empty is reported by the caller, not as failures"
        );
    }

    /// Minted ids are client-side, unique, and non-empty — the daemon never
    /// mints one (uncertain retry must be able to repeat THIS key).
    #[test]
    fn minted_request_ids_are_unique_and_nonempty() {
        let a = mint_request_id();
        let b = mint_request_id();
        assert!(!a.is_empty());
        assert_ne!(a, b);
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

    /// A recording fake daemon: every request's method+path+body is captured so
    /// a test can prove what the CLI did — and, more importantly, what it did
    /// NOT do (no consume POST on a refused/cancelled redemption).
    struct FakeDaemon {
        endpoint: Endpoint,
        seen: std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
    }

    impl FakeDaemon {
        async fn start(credits: serde_json::Value, consume: serde_json::Value) -> Self {
            Self::start_with_status(credits, axum::http::StatusCode::OK, consume).await
        }

        async fn start_with_status(
            credits: serde_json::Value,
            consume_status: axum::http::StatusCode,
            consume: serde_json::Value,
        ) -> Self {
            use axum::routing::{get, post};
            use axum::Router;
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let rec = seen.clone();
            let rec2 = seen.clone();
            let app = Router::new()
                .route(
                    "/llmux/reset-credits",
                    get(move |uri: axum::http::Uri| {
                        let rec = rec.clone();
                        let credits = credits.clone();
                        async move {
                            rec.lock().unwrap().push((
                                "GET".into(),
                                uri.to_string(),
                                String::new(),
                            ));
                            axum::Json(credits)
                        }
                    }),
                )
                .route(
                    "/llmux/reset-credits/consume",
                    post(move |body: String| {
                        let rec = rec2.clone();
                        let consume = consume.clone();
                        async move {
                            rec.lock().unwrap().push((
                                "POST".into(),
                                "/llmux/reset-credits/consume".into(),
                                body,
                            ));
                            (consume_status, axum::Json(consume))
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            FakeDaemon {
                endpoint: Endpoint {
                    base_url: format!("http://127.0.0.1:{port}"),
                    api_key: Some("lm-test".into()),
                    remote: true,
                    host: "127.0.0.1".into(),
                    port,
                },
                seen,
            }
        }

        fn consume_posts(&self) -> Vec<String> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .filter(|(method, _, _)| method == "POST")
                .map(|(_, _, body)| body.clone())
                .collect()
        }

        fn requests(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
    }

    fn reset_args(account: &str) -> crate::cli::AccountsResetArgs {
        crate::cli::AccountsResetArgs {
            account: account.into(),
            credit_id: None,
            request_id: None,
            yes: false,
        }
    }

    /// Non-TTY without `--yes` refuses BEFORE any network call — the cancelled
    /// redemption spends nothing and the daemon never sees a consume POST.
    #[tokio::test]
    async fn reset_without_confirmation_never_touches_the_daemon() {
        let fake = FakeDaemon::start(
            serde_json::json!({ "account": "codex:a@x.com", "available_count": 3,
                                "credits": [], "redeemable": true }),
            serde_json::json!({ "outcome": "reset", "request_id": "x", "windows_reset": 1 }),
        )
        .await;
        // cargo test's stdin is not a terminal → the non-TTY branch.
        let err = reset(&fake.endpoint, reset_args("codex:a@x.com"))
            .await
            .expect_err("refuses without --yes");
        let CliError::Message(msg) = err else {
            panic!("expected a Message error");
        };
        assert!(msg.contains("--yes"), "{msg}");
        assert_eq!(fake.requests(), 0, "no request at all before confirmation");
    }

    /// Zero owned resets refuses the NEW redemption after the fresh list read —
    /// and never POSTs a consume.
    #[tokio::test]
    async fn reset_refuses_when_no_reset_is_owned() {
        let fake = FakeDaemon::start(
            serde_json::json!({ "account": "codex:a@x.com", "available_count": 0,
                                "applicable_available_count": 0, "credits": [],
                                "redeemable": false }),
            serde_json::json!({ "outcome": "reset", "request_id": "x", "windows_reset": 1 }),
        )
        .await;
        let mut args = reset_args("codex:a@x.com");
        args.yes = true;
        let err = reset(&fake.endpoint, args).await.expect_err("no credit");
        let CliError::Message(msg) = err else {
            panic!("expected a Message error");
        };
        assert!(msg.contains("no redeemable reset"), "{msg}");
        assert!(
            fake.consume_posts().is_empty(),
            "a refused redemption never posts"
        );
    }

    /// An explicit `--request-id` is a RETRY of a possibly-already-executed
    /// redemption: it reuses that exact key (never mints a new one) and does
    /// not re-gate on a fresh inventory read, because the first attempt may
    /// already have consumed the credit.
    #[tokio::test]
    async fn reset_retry_reuses_the_given_request_id_without_regating() {
        let fake = FakeDaemon::start(
            // Inventory now reads zero — a retry must NOT be blocked by it.
            serde_json::json!({ "account": "codex:a@x.com", "available_count": 0,
                                "credits": [], "redeemable": false }),
            serde_json::json!({ "outcome": "already_redeemed", "request_id": "01RETRY",
                                "windows_reset": 0 }),
        )
        .await;
        let mut args = reset_args("codex:a@x.com");
        args.yes = true;
        args.request_id = Some("01RETRY".into());
        reset(&fake.endpoint, args).await.expect("retry succeeds");
        let posts = fake.consume_posts();
        assert_eq!(posts.len(), 1, "one consume POST");
        let body: serde_json::Value = serde_json::from_str(&posts[0]).expect("json body");
        assert_eq!(body["redeem_request_id"], "01RETRY", "same key reused");
        assert_eq!(body["confirm"], true);
        assert_eq!(body["account"], "codex:a@x.com");
        assert!(
            body.get("credit_id").is_none(),
            "an unset credit id is omitted, never serialized null: {body}"
        );
    }

    /// `accounts refresh` with no account name asks the daemon to refresh
    /// EVERY supported account (the key is omitted, not null), and a partial
    /// failure fails the command instead of reading as green.
    #[tokio::test]
    async fn refresh_all_accounts_reports_partial_failure_as_an_error() {
        use axum::routing::post;
        use axum::Router;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let rec = seen.clone();
        let app = Router::new().route(
            "/llmux/refresh-usage",
            post(move |body: String| {
                let rec = rec.clone();
                async move {
                    rec.lock().unwrap().push(body);
                    axum::Json(serde_json::json!({
                        "ok": false,
                        "results": [
                            { "account": "codex:a@x.com", "ok": true,
                              "usage_control": { "available_resets": 3,
                                                 "applicable_resets": 0 } },
                            { "account": "claude:b@x.com", "ok": false, "error": "upstream 502" },
                        ],
                    }))
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
            api_key: Some("lm-test".into()),
            remote: true,
            host: "127.0.0.1".into(),
            port,
        };
        let err = refresh(&endpoint, None).await.expect_err("partial failure");
        let CliError::Message(msg) = err else {
            panic!("expected a Message error");
        };
        assert!(msg.contains("claude:b@x.com"), "{msg}");
        let body: serde_json::Value = serde_json::from_str(&seen.lock().unwrap()[0]).unwrap();
        assert!(
            body.get("account").is_none(),
            "all-accounts refresh omits the key: {body}"
        );
    }

    /// An explicit `--credit-id` that names no redeemable credit refuses
    /// BEFORE the consume POST — nothing is spent on a typo.
    #[tokio::test]
    async fn reset_with_an_unknown_credit_id_never_posts() {
        let fake = FakeDaemon::start(
            serde_json::json!({ "account": "codex:a@x.com", "available_count": 2,
                                "redeemable": true, "credits": [
                { "id": "c1", "reset_type": "codex_rate_limits", "status": "available",
                  "title": "Title A" }
            ]}),
            serde_json::json!({ "outcome": "reset", "request_id": "x", "windows_reset": 1 }),
        )
        .await;
        let mut args = reset_args("codex:a@x.com");
        args.yes = true;
        args.credit_id = Some("does-not-exist".into());
        let err = fake_reset_err(&fake, args).await;
        assert!(err.contains("does-not-exist"), "{err}");
        assert!(
            fake.consume_posts().is_empty(),
            "a typo'd credit id spends nothing"
        );
    }

    /// The credit the operator CONFIRMED is the credit the POST names: when
    /// the inventory rows carry ids, the chosen row's id is bound to the
    /// request instead of leaving the daemon to pick again independently.
    #[tokio::test]
    async fn reset_binds_the_confirmed_credit_to_the_consume_request() {
        let fake = FakeDaemon::start(
            serde_json::json!({ "account": "codex:a@x.com", "available_count": 2,
                                "redeemable": true, "credits": [
                { "id": "c1", "reset_type": "codex_rate_limits", "status": "available",
                  "title": "Title A" },
                { "id": "c2", "reset_type": "codex_rate_limits", "status": "available",
                  "title": "Title B" }
            ]}),
            serde_json::json!({ "outcome": "reset", "request_id": "01RID", "windows_reset": 1 }),
        )
        .await;
        let mut args = reset_args("codex:a@x.com");
        args.yes = true;
        reset(&fake.endpoint, args).await.expect("redeemed");
        let posts = fake.consume_posts();
        assert_eq!(posts.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&posts[0]).expect("json body");
        assert_eq!(
            body["credit_id"], "c1",
            "the confirmed credit is the one sent: {body}"
        );

        // And an explicit id rides through unchanged.
        let fake = FakeDaemon::start(
            serde_json::json!({ "account": "codex:a@x.com", "available_count": 2,
                                "redeemable": true, "credits": [
                { "id": "c1", "reset_type": "codex_rate_limits", "status": "available",
                  "title": "Title A" },
                { "id": "c2", "reset_type": "codex_rate_limits", "status": "available",
                  "title": "Title B" }
            ]}),
            serde_json::json!({ "outcome": "reset", "request_id": "01RID", "windows_reset": 1 }),
        )
        .await;
        let mut args = reset_args("codex:a@x.com");
        args.yes = true;
        args.credit_id = Some("c2".into());
        reset(&fake.endpoint, args).await.expect("redeemed");
        let body: serde_json::Value =
            serde_json::from_str(&fake.consume_posts()[0]).expect("json body");
        assert_eq!(body["credit_id"], "c2", "{body}");
    }

    /// Run `reset` expecting a refusal, returning its message.
    async fn fake_reset_err(fake: &FakeDaemon, args: crate::cli::AccountsResetArgs) -> String {
        match reset(&fake.endpoint, args).await {
            Err(CliError::Message(msg)) => msg,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// A daemon that reports a DIFFERENT pending request id for this account
    /// (its persisted pending receipt) is a conflict: the client surfaces that
    /// id for an explicit retry instead of starting a second redemption.
    #[tokio::test]
    async fn reset_surfaces_a_conflicting_pending_request_id() {
        let fake = FakeDaemon::start_with_status(
            serde_json::json!({ "account": "codex:a@x.com", "available_count": 3,
                                "redeemable": true, "credits": [
                { "reset_type": "codex_rate_limits", "status": "available", "title": "Full reset" }
            ]}),
            axum::http::StatusCode::CONFLICT,
            serde_json::json!({
                "type": "error",
                "error": { "type": "pending_redemption",
                           "message": "account codex:a@x.com has a pending redemption \
                                       (request id 01PENDING)" },
                "pending_request_id": "01PENDING",
            }),
        )
        .await;
        let mut args = reset_args("codex:a@x.com");
        args.yes = true;
        let err = reset(&fake.endpoint, args)
            .await
            .expect_err("conflicting pending id");
        let CliError::Message(msg) = err else {
            panic!("expected a Message error");
        };
        assert!(msg.contains("01PENDING"), "{msg}");
        assert!(msg.contains("--request-id 01PENDING"), "{msg}");
    }
}
