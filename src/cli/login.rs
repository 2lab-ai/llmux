//! `llmux login [--api | --codex | --grok | --openrouter | --gemini]` — add
//! an account.

use std::path::Path;

use crate::auth::{codex, gemini, oauth, openrouter, profile, AuthError};
use crate::config::{AccountConfig, AccountCredential, Config, GeminiConfig, Upsert};

use super::{prompt_line, CliError, LoginArgs};

/// OAuth path: PKCE browser flow → profile fetch (accountUuid, email) →
/// upsert into config by `account_uuid` (FR2 dedup).
/// `--api` path: prompt for an API key, store as an apikey account.
/// `--codex` path: ChatGPT OAuth browser flow → upsert a Codex account.
/// `--grok` path: xAI device-code flow → upsert a Grok account.
/// `--openrouter` path: OpenRouter PKCE browser flow (or `--paste`) →
/// upsert an `or:<label>` account.
/// `--gemini` path: Google (Antigravity) OAuth browser flow → upsert a
/// `gemini:{email}` account.
pub async fn run(args: LoginArgs) -> Result<(), CliError> {
    if args.gemini {
        login_gemini().await
    } else if args.openrouter {
        login_openrouter(args.paste).await
    } else if args.grok {
        login_grok().await
    } else if args.codex {
        login_codex().await
    } else if args.api {
        login_api().await
    } else {
        login_oauth().await
    }
}

async fn login_api() -> Result<(), CliError> {
    let api_key = prompt_line("Anthropic API key: ")?;
    if api_key.is_empty() {
        return Err(CliError::Message("no API key provided".into()));
    }

    let mut name = String::new();
    crate::config::update(|config: &mut Config| {
        let n = config
            .accounts
            .iter()
            .filter(|a| a.name.starts_with("api-"))
            .count()
            + 1;
        name = format!("api-{n}");
        config.upsert_account(AccountConfig {
            name: name.clone(),
            credential: AccountCredential::Apikey {
                api_key: api_key.clone(),
            },
        });
    })?;

    println!("Added API key account {name:?}");
    println!("Saved to {}", crate::config::config_path()?.display());
    Ok(())
}

async fn login_oauth() -> Result<(), CliError> {
    let config = crate::config::load_or_init()?;
    let client = reqwest::Client::new();

    println!("Starting OAuth login...");
    let account = oauth_login_to_account(&client, &config.upstream).await?;

    let mut final_name = account.name.clone();
    let mut outcome = Upsert::Added;
    crate::config::update(|config: &mut Config| {
        let account = account.clone();
        outcome = config.upsert_account(account.clone());
        // Report the name the ROSTER ended up with: an identity-matched
        // re-login keeps the established name (`docs/keys-history/
        // relogin-trace.md` B6), so echoing the freshly derived one would name
        // an account that exists nowhere.
        final_name = config
            .find_account(&account)
            .map(|idx| config.accounts[idx].name.clone())
            .unwrap_or(account.name);
    })?;

    match outcome {
        Upsert::Added => println!("Added account {final_name:?}"),
        Upsert::Updated => println!("Updated account {final_name:?}"),
    }
    println!("Saved to {}", crate::config::config_path()?.display());
    Ok(())
}

/// Run the Anthropic PKCE browser flow and turn the result into a
/// ready-to-upsert [`AccountConfig`] — the shared core of the CLI `login`
/// command AND the dashboard's "new login from the switcher" path (issue #4),
/// so both build the identical `claude:<email>` account from the same flow.
///
/// `upstream` is the base URL the profile fetch (`/api/oauth/profile`) hits.
/// A profile-fetch failure is an ERROR: the account would have no stable
/// identity to dedup on, and persisting it is what duplicated accounts
/// (`docs/keys-history/relogin-trace.md` B7). This function performs NO
/// config write and NO logging of the token — the caller persists it (CLI:
/// `config::update`; dashboard: `AppState::inject_account` /
/// `POST /llmux/inject-account`).
pub async fn oauth_login_to_account(
    client: &reqwest::Client,
    upstream: &str,
) -> Result<AccountConfig, CliError> {
    let tokens = oauth::login_interactive(client).await?;

    // The profile fetch IS the identity step: it yields the `accountUuid` every
    // later upsert dedups on. A failure here is fatal on purpose — persisting
    // an unidentified account is what produced duplicate `claude:account-N`
    // rows (`docs/keys-history/relogin-trace.md` B7). The freshly minted tokens
    // are dropped with it; re-running the login is cheap, an un-dedupable
    // account is not.
    let profile = profile::fetch_profile(client, upstream, &tokens.access_token)
        .await
        .map_err(|err| {
            CliError::Message(format!(
                "could not fetch the account profile ({err}); nothing was saved — re-run the login"
            ))
        })?;
    if let Some(tier) = &profile.tier {
        println!("Detected Claude {tier} account: {}", profile.email);
    }
    let name = claude_account_name(&profile.account_uuid, &profile.email)?;

    Ok(AccountConfig {
        name,
        credential: AccountCredential::Oauth {
            account_uuid: profile.account_uuid,
            access_token: tokens.access_token,
            // A fresh code exchange always carries a refresh token; `None`
            // (refresh-style response) degrades to empty.
            refresh_token: tokens.refresh_token.unwrap_or_default(),
            expires_at_ms: tokens.expires_at_ms,
            tier: profile.tier,
            // Login mints a brand-new token — that IS a refresh for the
            // dashboard's "refreshed ago" display.
            last_refresh_ms: Some(super::now_ms()),
        },
    })
}

/// Local label for a freshly logged-in Claude account — and the gate that
/// refuses an UNIDENTIFIED one (`docs/keys-history/relogin-trace.md` B7).
///
/// The `account_uuid` is the account's only stable identity: `upsert_account`
/// dedups on it, and `AccountCredential::account_uuid()` reports `None` when it
/// is empty. Persisting an account without one means every later login either
/// overwrites it by name or piles up another duplicate — so an empty uuid is a
/// hard error, not a placeholder name. Identity is NEVER inferred from the
/// token (a token is not an identity and it rotates); with a uuid but no email
/// the uuid itself is the label.
///
/// The model group is encoded in the name (`claude:…`) so one email can hold a
/// Claude AND a Codex subscription without colliding (req5).
pub(crate) fn claude_account_name(account_uuid: &str, email: &str) -> Result<String, CliError> {
    if account_uuid.trim().is_empty() {
        return Err(CliError::Message(
            "could not identify the account (no accountUuid in the profile); nothing was saved — re-run the login. An account with no stable identity cannot be deduped, so a later re-login would duplicate it instead of updating it."
                .into(),
        ));
    }
    Ok(if email.is_empty() {
        format!("claude:{account_uuid}")
    } else {
        format!("claude:{email}")
    })
}

/// `--codex`: run the ChatGPT OAuth browser flow and upsert a Codex account.
/// Falls back to importing `~/.codex/auth.json` (renamed to the
/// `codex:{email}` convention) when the interactive flow cannot run.
async fn login_codex() -> Result<(), CliError> {
    let config = crate::config::load_or_init()?;
    let client = reqwest::Client::new();

    println!("Starting ChatGPT (Codex) OAuth login...");
    let account = match codex::login_codex_interactive(&client, &config.codex.token_url).await {
        Ok(account) => account,
        Err(err) => {
            // Headless / no-browser / port-bind failures degrade to importing
            // the codex CLI's own credential store, still renamed to the
            // `codex:{email}` convention so it never collides with a Claude
            // account of the same email.
            eprintln!("warning: interactive ChatGPT login failed ({err})");
            account_from_codex_import()?.ok_or_else(|| {
                CliError::Message(
                    "interactive ChatGPT login failed and no ~/.codex/auth.json was found to \
                         import — run `codex login` first, or retry with a browser available"
                        .into(),
                )
            })?
        }
    };

    let final_name = account.name.clone();
    let mut outcome = Upsert::Added;
    crate::config::update(|config: &mut Config| {
        outcome = config.upsert_account(account.clone());
    })?;

    match outcome {
        Upsert::Added => println!("Added codex account {final_name:?}"),
        Upsert::Updated => println!("Updated codex account {final_name:?}"),
    }
    println!("Saved to {}", crate::config::config_path()?.display());
    Ok(())
}

/// `--gemini`: run the Google (Antigravity) OAuth browser flow and upsert a
/// `gemini:{email}` account (docs/gemini/spec.md §R2, trace S4). Falls back
/// to importing the Antigravity CLI token store when the interactive flow
/// cannot run (port 51121 taken, headless) — codex parity.
async fn login_gemini() -> Result<(), CliError> {
    let config = crate::config::load_or_init()?;
    let client = reqwest::Client::new();
    // The operator-configured OAuth client, first: without it there is no
    // login to run, and no fallback either — an imported account could not
    // refresh its token without the same client.
    let oauth = config.gemini.oauth_client()?;

    // Shown BEFORE the browser opens: the user decides with the risk in view.
    println!("{}", gemini::GEMINI_TOS_NOTE);
    println!("Starting Google (Antigravity) OAuth login...");
    let account = match gemini::login_gemini_interactive(
        &client,
        &oauth,
        &config.gemini.token_url,
        &config.gemini.upstream,
        &config.gemini.user_agent,
    )
    .await
    {
        Ok(account) => account,
        Err(err) => {
            // Headless / no-browser / port-bind failures degrade to importing
            // the Antigravity CLI's own token store (already named
            // `gemini:{email}`, the same name an OAuth login produces).
            eprintln!("warning: interactive Google login failed ({err})");
            account_from_antigravity_import(&client, &config.gemini)
                .await?
                .ok_or_else(|| {
                    CliError::Message(
                        "interactive Google login failed and no Antigravity CLI token store \
                     (~/.gemini/antigravity-cli/antigravity-oauth-token) was found to import \
                     — run `agy` once to sign in, or retry with a browser available"
                            .into(),
                    )
                })?
        }
    };

    let final_name = account.name.clone();
    let mut outcome = Upsert::Added;
    crate::config::update(|config: &mut Config| {
        outcome = config.upsert_account(account.clone());
    })?;

    match outcome {
        Upsert::Added => println!("Added gemini account {final_name:?}"),
        Upsert::Updated => println!("Updated gemini account {final_name:?}"),
    }
    println!("Saved to {}", crate::config::config_path()?.display());
    Ok(())
}

/// `--grok`: run the xAI device-code flow (docs/grok/spec.md T1) and upsert
/// a Grok account. No localhost callback: prints the verification URL + user
/// code, best-effort opens the browser, then polls the token endpoint.
async fn login_grok() -> Result<(), CliError> {
    crate::config::load_or_init()?;
    // No-redirect client: the poll POST returns the refresh token, so a
    // redirect to an off-boundary host must never resend it (review round 3).
    let client = crate::auth::grok::oauth_http_client();

    println!("Starting xAI (Grok) device-code login...");
    let discovery = crate::auth::grok::discover(&client).await?;
    let device = crate::auth::grok::request_device_code(&client, &discovery).await?;
    println!("Open:  {}", device.open_url());
    println!("Code:  {}", device.user_code);
    oauth::open_browser(device.open_url());
    println!("Waiting for authorization (Ctrl-C to abort)...");
    let bundle = crate::auth::grok::poll_token(&client, &discovery.token_endpoint, &device).await?;
    let account = crate::auth::grok::account_from_bundle(&bundle, &discovery.token_endpoint)?;

    let final_name = account.name.clone();
    let mut outcome = Upsert::Added;
    crate::config::update(|config: &mut Config| {
        outcome = config.upsert_account(account.clone());
    })?;

    match outcome {
        Upsert::Added => println!("Added grok account {final_name:?}"),
        Upsert::Updated => println!("Updated grok account {final_name:?}"),
    }
    println!("Saved to {}", crate::config::config_path()?.display());
    Ok(())
}

/// `--openrouter`: mint (or accept) an OpenRouter API key and upsert an
/// `or:<label>` account (docs/openrouter/spec.md §R5).
///
/// `paste = true` skips the browser and prompts for an existing key; the
/// browser flow also degrades to that prompt when it cannot run at all
/// (no browser / callback timeout), so a headless box is never a dead end.
/// The key itself is never printed — only the derived account name is.
/// Account name for an OpenRouter login: `or:<key label>` when introspection
/// returned one, else `or:key-N` numbered against `config`'s existing
/// `or:key-*` accounts (same rule as `api-N` / `claude:account-N`).
///
/// SHARED with the daemon-side login (`proxy::server::run_login`) so the CLI
/// and the dashboard switcher cannot drift into two naming schemes — the first
/// cut of the daemon arm hardcoded a label-less `or:key`, which would have made
/// every unlabeled dashboard login overwrite the previous one.
/// Whether an OpenRouter key label is really just the key (masked or not).
/// Their `/api/v1/key` returns the key itself as the "label" for a
/// PKCE-minted key, so this is the common case, not an edge case.
pub(crate) fn label_is_key_shaped(label: &str) -> bool {
    let lower = label.trim().to_ascii_lowercase();
    lower.starts_with("sk-or-") || lower.starts_with("sk-")
}

pub(crate) fn openrouter_account_name(config: &Config, label: &str, api_key: &str) -> String {
    // The KEY is the durable identity; the label is cosmetic and can change
    // between logins (introspection is best-effort and degrades to empty). So
    // look for this key FIRST, under whatever name it already has: without
    // this, a key stored as `or:key` because the label lookup failed comes
    // back as `or:work` once the label resolves, and one upstream credential
    // becomes two scheduler accounts — double-counting its quota.
    if let Some(existing) = config.accounts.iter().find(|a| {
        matches!(
            &a.credential,
            AccountCredential::OpenRouter { api_key: k, .. } if k == api_key
        )
    }) {
        return existing.name.clone();
    }

    let taken_by_another_key = |name: &str| {
        config.accounts.iter().any(|a| {
            a.name == name
                && !matches!(
                    &a.credential,
                    AccountCredential::OpenRouter { api_key: existing, .. } if existing == api_key
                )
        })
    };

    let base = {
        let label = label.trim();
        // OpenRouter's key introspection does NOT return a human label for a
        // PKCE-minted key — it returns the KEY, masked: a real login produced
        // `sk-or-v1-010...e42` (live 2026-08-22), which would have made the
        // account `or:sk-or-v1-010...e42`. That is key material in a name that
        // then appears in status output, logs and the TUI, and it identifies
        // nothing a human would recognize. Treat a key-shaped label as absent
        // and fall back to the numbered form.
        if label.is_empty() || label_is_key_shaped(label) {
            "or:key".to_string()
        } else {
            format!("or:{label}")
        }
    };
    // Re-logging in with the SAME key keeps the same name (an in-place
    // update); a DIFFERENT key never lands on an occupied name.
    if !taken_by_another_key(&base) {
        return base;
    }
    // First UNUSED suffix, not `count + 1`: with `or:key-1` and `or:key-3`
    // present, counting yields `or:key-3` and the name-keyed upsert would
    // REPLACE that account — silently destroying a working credential instead
    // of growing the pool. OpenRouter key labels are explicitly not unique
    // (see `AccountCredential::OpenRouter`), so this collision is reachable
    // with labels too, not just unlabeled logins.
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|name| !taken_by_another_key(name))
        .unwrap_or(base)
}

async fn login_openrouter(paste: bool) -> Result<(), CliError> {
    crate::config::load_or_init()?;
    let client = reqwest::Client::new();

    let api_key = if paste {
        prompt_openrouter_key()?
    } else {
        println!("Starting OpenRouter OAuth login...");
        match openrouter::login_interactive(&client).await {
            Ok(key) => key,
            // Aborted/Io = the flow could not complete locally (no browser,
            // timeout, port bind). A token-endpoint or transport failure is a
            // real error and propagates instead of silently asking for a key.
            Err(err @ (AuthError::Aborted(_) | AuthError::Io(_))) => {
                eprintln!("warning: interactive OpenRouter login failed ({err})");
                eprintln!("falling back to manual key entry");
                prompt_openrouter_key()?
            }
            Err(err) => return Err(err.into()),
        }
    };

    // Cosmetic only: a failed introspection degrades to the `or:key-N` name.
    // A key-shaped "label" is discarded outright rather than persisted — it is
    // the key again, not a name, and storing it just duplicates the secret.
    let label = openrouter::fetch_key_label(&client, openrouter::KEY_INFO_URL, &api_key)
        .await
        .filter(|l| !label_is_key_shaped(l))
        .unwrap_or_default();

    let mut final_name = String::new();
    let mut outcome = Upsert::Added;
    crate::config::update(|config: &mut Config| {
        // Numbering is resolved against the fresh on-disk state so unlabeled
        // logins don't overwrite each other (same rule as `api-N`).
        let name = openrouter_account_name(config, &label, &api_key);
        final_name = name.clone();
        outcome = config.upsert_account(AccountConfig {
            name,
            credential: AccountCredential::OpenRouter {
                api_key: api_key.clone(),
                label: label.clone(),
            },
        });
    })?;

    match outcome {
        Upsert::Added => println!("Added account {final_name:?}"),
        Upsert::Updated => println!("Updated account {final_name:?}"),
    }
    println!("Saved to {}", crate::config::config_path()?.display());
    Ok(())
}

/// Prompt for an `sk-or-v1-…` key (stderr prompt, stdin read) — the shape
/// `login_api` uses. Echoing the key back is deliberately not done.
fn prompt_openrouter_key() -> Result<String, CliError> {
    let api_key = prompt_line("OpenRouter API key (sk-or-v1-…): ")?;
    if api_key.is_empty() {
        return Err(CliError::Message("no API key provided".into()));
    }
    Ok(api_key)
}

/// Import `~/.codex/auth.json` (when present) and rename it to the
/// `codex:{email}` convention. `Ok(None)` when no auth.json exists.
fn account_from_codex_import() -> Result<Option<AccountConfig>, CliError> {
    let Some(path) = codex::default_codex_auth_path() else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let mut account = codex::import_codex_auth(&path)?;
    // `import_codex_auth` names the account after the raw email (or "codex");
    // re-derive the `codex:{email}` name so imports match OAuth logins.
    let account_id = account
        .credential
        .account_uuid()
        .unwrap_or_default()
        .to_string();
    let email = (account.name != "codex").then_some(account.name.as_str());
    account.name = codex::codex_account_name(email, &account_id);
    Ok(Some(account))
}

/// Import the Antigravity CLI token store (when present) as a
/// `gemini:{email}` account. `Ok(None)` when no store exists.
async fn account_from_antigravity_import(
    client: &reqwest::Client,
    gemini_config: &GeminiConfig,
) -> Result<Option<AccountConfig>, CliError> {
    let Some(path) = gemini::antigravity_store_path() else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    read_antigravity_store(&path, client, gemini_config)
        .await
        .map(Some)
}

/// Read and parse one Antigravity CLI token store file
/// ([`gemini::account_from_antigravity_store`]), then resolve its Cloud Code
/// project eagerly when the stored access token is still valid
/// ([`resolve_store_project`]). Shared by the login fallback and the
/// `llmux import` probe so both build the identical account.
pub(super) async fn read_antigravity_store(
    path: &Path,
    client: &reqwest::Client,
    gemini_config: &GeminiConfig,
) -> Result<AccountConfig, CliError> {
    let bytes = std::fs::read(path)?;
    let mut account = gemini::account_from_antigravity_store(&bytes)?;
    if let Some(warning) = client_missing_warning(gemini_config.oauth_client()) {
        eprintln!("{warning}");
    }
    if let Some(warning) =
        resolve_store_project(&mut account, client, gemini_config, now_epoch_ms()).await
    {
        eprintln!("{warning}");
    }
    Ok(account)
}

/// The import-time warning for a store imported while no gemini OAuth client
/// is configured: the import itself is offline and still succeeds, but the
/// daemon cannot refresh the token it carries. `None` when the client
/// resolves.
fn client_missing_warning(
    resolved: Result<gemini::GeminiOAuthClient, crate::auth::AuthError>,
) -> Option<String> {
    resolved.err().map(|err| {
        format!(
            "warning: {err}; this account will stop working when its current token expires (~1h)"
        )
    })
}

/// Remaining access-token lifetime below which an imported token is treated
/// as expired: not worth a lookup that may outlive it.
const STORE_TOKEN_MIN_REMAINING_MS: u64 = 60_000;

/// Eager project lookup for an imported store account (trace S5): when the
/// stored access token is still valid (`expires_at_ms` more than
/// [`STORE_TOKEN_MIN_REMAINING_MS`] past `now_ms`), resolve the project with
/// the same `loadCodeAssist` call an interactive login makes, against the
/// CONFIGURED Cloud Code base and identity. Never refreshes: an expired or
/// unknown-expiry token keeps the project empty for the daemon to resolve
/// (the first model request, or the next usage refresh once the token is
/// fresh). Returns the warning to print when the lookup failed (the project
/// stays empty then too).
async fn resolve_store_project(
    account: &mut AccountConfig,
    client: &reqwest::Client,
    gemini_config: &GeminiConfig,
    now_ms: u64,
) -> Option<String> {
    let AccountCredential::Gemini {
        access_token,
        expires_at_ms,
        project_id,
        ..
    } = &mut account.credential
    else {
        return None;
    };
    if !project_id.is_empty()
        || *expires_at_ms <= now_ms.saturating_add(STORE_TOKEN_MIN_REMAINING_MS)
    {
        return None;
    }
    match gemini::resolve_project(
        client,
        &gemini_config.upstream,
        &gemini_config.user_agent,
        access_token,
    )
    .await
    {
        Ok(project) => {
            *project_id = project;
            None
        }
        Err(err) => Some(gemini::project_unresolved_warning(&err)),
    }
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `docs/keys-history/relogin-trace.md` B7: an OAuth login whose profile
    /// could not be identified must FAIL, not persist an account with no
    /// stable identity. `AccountCredential::account_uuid()` returns `None` for
    /// an empty uuid, so such an account can only dedup by name — the next
    /// unidentified login either collides with it or piles up a duplicate.
    #[test]
    fn an_unidentified_oauth_login_is_refused() {
        let err =
            claude_account_name("", "someone@x.com").expect_err("no stable identity => no account");
        assert!(
            err.to_string().contains("identify"),
            "the error must say WHY nothing was saved: {err}"
        );
        assert!(claude_account_name("", "").is_err());
    }

    /// An identified profile names the account after its email…
    #[test]
    fn an_identified_login_is_named_after_its_email() {
        assert_eq!(
            claude_account_name("uuid-a", "me@x.com").expect("identified"),
            "claude:me@x.com"
        );
    }

    /// …and an identified profile with NO email falls back to the uuid — a
    /// real stable identity. Never a token-derived value, and never a shared
    /// placeholder two different accounts could both land on.
    #[test]
    fn an_identified_login_without_an_email_falls_back_to_its_uuid() {
        let name = claude_account_name("uuid-a", "").expect("identified");
        assert_eq!(name, "claude:uuid-a");
        assert_ne!(name, "claude:account", "no shared placeholder");
    }

    fn or_account(name: &str, key: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            credential: AccountCredential::OpenRouter {
                api_key: key.to_string(),
                label: String::new(),
            },
        }
    }

    /// Re-logging in with the SAME key must reuse the name (in-place update),
    /// while a DIFFERENT key must never land on an occupied one.
    #[test]
    fn openrouter_name_reuses_for_the_same_key_and_avoids_another() {
        let mut config = Config::default();
        config.accounts.push(or_account("or:work", "sk-or-v1-aaa"));

        assert_eq!(
            openrouter_account_name(&config, "work", "sk-or-v1-aaa"),
            "or:work",
            "same key re-login updates in place"
        );
        assert_eq!(
            openrouter_account_name(&config, "work", "sk-or-v1-bbb"),
            "or:work-2",
            "a different key with the SAME label must not overwrite it \
             (openrouter labels are explicitly not unique)"
        );
    }

    /// The key, not the label, is the identity. A label that appears (or
    /// changes) between logins must NOT mint a second account for the same
    /// credential — that would show one upstream quota as two scheduler
    /// accounts.
    #[test]
    fn openrouter_name_follows_the_key_when_the_label_changes() {
        let mut config = Config::default();
        // Stored unlabeled first (introspection failed).
        config.accounts.push(or_account("or:key", "sk-or-v1-aaa"));

        assert_eq!(
            openrouter_account_name(&config, "work", "sk-or-v1-aaa"),
            "or:key",
            "same key keeps its existing name even once a label resolves"
        );
        assert_eq!(
            openrouter_account_name(&config, "", "sk-or-v1-aaa"),
            "or:key",
            "and when the label disappears again"
        );
        // A genuinely new key is unaffected.
        assert_eq!(
            openrouter_account_name(&config, "work", "sk-or-v1-bbb"),
            "or:work"
        );
    }

    /// OpenRouter returns the KEY (masked) as the label for a PKCE-minted
    /// key — a real login produced `sk-or-v1-010...e42`. Naming the account
    /// after that puts key material into status output, logs and the TUI and
    /// identifies nothing. Found by the live smoke, not by any mock.
    #[test]
    fn a_key_shaped_label_is_not_used_as_the_account_name() {
        let config = Config::default();
        for label in ["sk-or-v1-010...e42", "sk-or-v1-abcdef", "sk-ant-whatever"] {
            let name = openrouter_account_name(&config, label, "sk-or-v1-AAA");
            assert_eq!(name, "or:key", "{label} must not become the name");
            assert!(!name.contains("sk-"), "no key material in the name: {name}");
        }
        // A genuine human label is still used.
        assert_eq!(
            openrouter_account_name(&config, "work laptop", "sk-or-v1-AAA"),
            "or:work laptop"
        );
    }

    /// The unlabeled fallback must pick the first UNUSED suffix. Counting
    /// existing accounts instead would return an occupied name after a gap,
    /// and the name-keyed upsert would destroy that credential.
    #[test]
    fn openrouter_unlabeled_name_skips_gaps_instead_of_overwriting() {
        let mut config = Config::default();
        config.accounts.push(or_account("or:key", "sk-or-v1-1"));
        config.accounts.push(or_account("or:key-3", "sk-or-v1-3"));

        // `or:key` and `or:key-3` are taken; `or:key-2` is the first free slot.
        assert_eq!(
            openrouter_account_name(&config, "", "sk-or-v1-new"),
            "or:key-2"
        );
        // Filling the gap pushes the next one past the highest existing.
        config.accounts.push(or_account("or:key-2", "sk-or-v1-2"));
        assert_eq!(
            openrouter_account_name(&config, "", "sk-or-v1-new"),
            "or:key-4"
        );
    }

    #[test]
    fn openrouter_name_is_the_bare_label_on_an_empty_config() {
        let config = Config::default();
        assert_eq!(
            openrouter_account_name(&config, "  my-key  ", "sk-or-v1-x"),
            "or:my-key",
            "label is trimmed"
        );
        assert_eq!(openrouter_account_name(&config, "", "sk-or-v1-x"), "or:key");
    }

    /// A `{"token":…, "id_token":<JWT with email>}` Antigravity CLI store
    /// whose access token expires at `expiry` (RFC 3339).
    fn antigravity_store_json(email: &str, expiry: &str) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let body = URL_SAFE_NO_PAD.encode(serde_json::json!({ "email": email }).to_string());
        serde_json::json!({
            "token": {
                "access_token": "ya29.STORE-AT",
                "token_type": "Bearer",
                "refresh_token": "1//STORE-RT",
                "expiry": expiry,
            },
            "auth_method": "oauth",
            "id_token": format!("{header}.{body}.sig"),
        })
        .to_string()
    }

    fn scratch_file(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "llmux-login-{tag}-{}-{}",
            std::process::id(),
            ulid::Ulid::new()
        ))
    }

    /// A store whose access token expired long ago: never looked up.
    const EXPIRED: &str = "2000-01-01T00:00:00Z";
    /// A store whose access token is still valid.
    const VALID: &str = "2999-01-01T00:00:00Z";

    /// What a local `loadCodeAssist` mock saw: `(path, authorization,
    /// user-agent)` per request.
    type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>;

    /// A local Cloud Code mock answering every POST with `status` + `body`.
    async fn spawn_cloud_code(status: u16, body: &'static str) -> (GeminiConfig, Seen) {
        let seen: Seen = Seen::default();
        let recorder = seen.clone();
        let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let recorder = recorder.clone();
            async move {
                let header = |name: &str| {
                    req.headers()
                        .get(name)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string()
                };
                recorder.lock().expect("seen lock").push((
                    req.uri().path().to_string(),
                    header("authorization"),
                    header("user-agent"),
                ));
                axum::http::Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body))
                    .expect("mock response")
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let config = GeminiConfig {
            upstream: format!("http://{addr}"),
            user_agent: "antigravity/hub/9.9.9 test/arm64".to_string(),
            ..GeminiConfig::default()
        };
        (config, seen)
    }

    async fn read_store(expiry: &str, config: &GeminiConfig) -> AccountConfig {
        let path = scratch_file("agy-store");
        std::fs::write(&path, antigravity_store_json("me@example.com", expiry))
            .expect("write store");
        let account = read_antigravity_store(&path, &reqwest::Client::new(), config).await;
        let _ = std::fs::remove_file(&path);
        account.expect("store parses")
    }

    fn project_of(account: &AccountConfig) -> &str {
        match &account.credential {
            AccountCredential::Gemini { project_id, .. } => project_id,
            other => panic!("gemini credential expected, got {other:?}"),
        }
    }

    /// trace S5: a store whose access token is still valid gets its project
    /// at import time — one `loadCodeAssist` with the stored token and the
    /// configured Antigravity identity, the same call an interactive login
    /// makes — instead of on the first request.
    #[tokio::test]
    async fn antigravity_store_with_a_valid_token_resolves_its_project() {
        let (config, seen) =
            spawn_cloud_code(200, r#"{"cloudaicompanionProject":"proj-eager"}"#).await;
        let account = read_store(VALID, &config).await;
        assert_eq!(project_of(&account), "proj-eager");
        let seen = seen.lock().expect("seen lock").clone();
        assert_eq!(
            seen,
            vec![(
                "/v1internal:loadCodeAssist".to_string(),
                "Bearer ya29.STORE-AT".to_string(),
                "antigravity/hub/9.9.9 test/arm64".to_string(),
            )]
        );
    }

    /// A store imported without a configured OAuth client still imports, but
    /// the operator is told up front that the daemon will not be able to
    /// refresh its token; a configured client prints nothing.
    #[test]
    fn store_import_warns_when_no_oauth_client_is_configured() {
        let warning = client_missing_warning(GeminiConfig::default().oauth_client_from(|_| None))
            .expect("a warning");
        assert!(
            warning.starts_with("warning: gemini OAuth client not configured: "),
            "{warning}"
        );
        assert!(
            warning
                .ends_with("; this account will stop working when its current token expires (~1h)"),
            "{warning}"
        );
        let configured = GeminiConfig {
            client_id: Some("test-client-id".into()),
            client_secret: Some("test-client-secret".into()),
            ..GeminiConfig::default()
        };
        assert_eq!(
            client_missing_warning(configured.oauth_client_from(|_| None)),
            None
        );
    }

    /// An expired stored token is never used and never refreshed at import:
    /// no call, project left for the first request.
    #[tokio::test]
    async fn antigravity_store_with_an_expired_token_makes_no_call() {
        let (config, seen) =
            spawn_cloud_code(200, r#"{"cloudaicompanionProject":"proj-eager"}"#).await;
        let account = read_store(EXPIRED, &config).await;
        assert_eq!(project_of(&account), "");
        assert!(seen.lock().expect("seen lock").is_empty(), "no lookup");
        // Inside the 60s margin counts as expired too.
        let mut account = read_store(EXPIRED, &config).await;
        if let AccountCredential::Gemini { expires_at_ms, .. } = &mut account.credential {
            *expires_at_ms = 1_000_000 + 59_999;
        }
        let warning =
            resolve_store_project(&mut account, &reqwest::Client::new(), &config, 1_000_000).await;
        assert_eq!(warning, None);
        assert!(seen.lock().expect("seen lock").is_empty(), "no lookup");
    }

    /// A failed lookup never fails the import: the project stays empty and
    /// the login's own warning text is returned for printing.
    #[tokio::test]
    async fn antigravity_store_lookup_failure_keeps_the_project_empty_with_a_warning() {
        let (config, seen) = spawn_cloud_code(500, r#"{"error":"boom"}"#).await;
        let path = scratch_file("agy-store");
        std::fs::write(&path, antigravity_store_json("me@example.com", VALID))
            .expect("write store");
        let bytes = std::fs::read(&path).expect("read store");
        let _ = std::fs::remove_file(&path);
        let mut account = gemini::account_from_antigravity_store(&bytes).expect("store parses");
        let warning = resolve_store_project(
            &mut account,
            &reqwest::Client::new(),
            &config,
            now_epoch_ms(),
        )
        .await
        .expect("a warning");
        assert!(
            warning.starts_with("warning: Cloud Code project not resolved yet (")
                && warning.ends_with(
                    "); llmux resolves it on the first request or the next usage refresh"
                ),
            "{warning}"
        );
        assert!(!warning.contains("STORE-AT"), "no token in it: {warning}");
        assert_eq!(project_of(&account), "");
        assert_eq!(seen.lock().expect("seen lock").len(), 1);
        // And through the shared reader: still an account, project empty.
        assert_eq!(project_of(&read_store(VALID, &config).await), "");
    }

    /// trace S5: the login fallback and `llmux import` read the store file
    /// into the same `gemini:{email}` account an OAuth login produces; an
    /// expired stored token leaves the project empty (resolved on the first
    /// request).
    #[tokio::test]
    async fn antigravity_store_file_reads_as_a_gemini_account() {
        let path = scratch_file("agy-store");
        std::fs::write(&path, antigravity_store_json("me@example.com", EXPIRED))
            .expect("write store");
        let account =
            read_antigravity_store(&path, &reqwest::Client::new(), &GeminiConfig::default()).await;
        let _ = std::fs::remove_file(&path);

        let account = account.expect("store parses");
        assert_eq!(account.name, "gemini:me@example.com");
        let AccountCredential::Gemini {
            email,
            access_token,
            refresh_token,
            project_id,
            ..
        } = account.credential
        else {
            panic!("gemini credential expected");
        };
        assert_eq!(email, "me@example.com");
        assert_eq!(access_token, "ya29.STORE-AT");
        assert_eq!(refresh_token, "1//STORE-RT");
        assert!(project_id.is_empty());
    }

    /// A missing or malformed store is an error from the path helper (the
    /// `llmux import` probe turns it into a warning) — and the error never
    /// carries a token value.
    #[tokio::test]
    async fn antigravity_store_file_errors_are_token_free() {
        let client = reqwest::Client::new();
        let config = GeminiConfig::default();
        let missing = scratch_file("agy-missing");
        assert!(read_antigravity_store(&missing, &client, &config)
            .await
            .is_err());

        let path = scratch_file("agy-no-refresh");
        std::fs::write(
            &path,
            r#"{"token":{"access_token":"ya29.SECRET-AT"},"id_token":"x.e30.sig"}"#,
        )
        .expect("write store");
        let err = read_antigravity_store(&path, &client, &config).await;
        let _ = std::fs::remove_file(&path);
        let err = err.expect_err("no refresh token => no account").to_string();
        assert!(err.contains("refresh_token"), "says why: {err}");
        assert!(!err.contains("SECRET"), "no token in the message: {err}");
    }
}
