//! `llmux login [--api | --codex]` — add an account.

use crate::auth::{codex, oauth, profile};
use crate::config::{AccountConfig, AccountCredential, Config, Upsert};

use super::{prompt_line, CliError, LoginArgs};

/// OAuth path: PKCE browser flow → profile fetch (accountUuid, email) →
/// upsert into config by `account_uuid` (FR2 dedup).
/// `--api` path: prompt for an API key, store as an apikey account.
/// `--codex` path: ChatGPT OAuth browser flow → upsert a Codex account.
pub async fn run(args: LoginArgs) -> Result<(), CliError> {
    if args.codex {
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
        let mut account = account.clone();
        // When the profile was unavailable the helper returns the placeholder
        // `claude:account`; assign the next free `claude:account-N` against the
        // fresh on-disk state so anonymous logins don't overwrite each other
        // (matches the original CLI behavior).
        if account.name == "claude:account" {
            let n = config
                .accounts
                .iter()
                .filter(|a| a.name.starts_with("claude:account-"))
                .count()
                + 1;
            account.name = format!("claude:account-{n}");
        }
        final_name = account.name.clone();
        outcome = config.upsert_account(account);
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
/// A profile-fetch failure degrades to an unenriched `claude:account-N` name
/// rather than losing the freshly minted tokens. This function performs NO
/// config write and NO logging of the token — the caller persists it (CLI:
/// `config::update`; dashboard: `AppState::inject_account` /
/// `POST /llmux/inject-account`).
pub async fn oauth_login_to_account(
    client: &reqwest::Client,
    upstream: &str,
) -> Result<AccountConfig, CliError> {
    let tokens = oauth::login_interactive(client).await?;

    // Profile fetch enriches uuid/name/tier; a failure degrades to an
    // unenriched account rather than losing the freshly minted tokens.
    let fetched = profile::fetch_profile(client, upstream, &tokens.access_token).await;
    let (account_uuid, email, tier) = match fetched {
        Ok(p) => {
            if let Some(tier) = &p.tier {
                println!("Detected Claude {tier} account: {}", p.email);
            }
            (p.account_uuid, p.email, p.tier)
        }
        Err(err) => {
            eprintln!("warning: could not fetch account profile — {err}");
            (String::new(), String::new(), None)
        }
    };

    // Encode the model group in the name (`claude:<email>`) so the same email
    // can hold a Claude AND a Codex subscription without colliding — mirrors
    // the `codex:<email>` convention the `--codex` flow uses (req5). When the
    // profile is unknown the name carries an empty uuid; the daemon's upsert
    // then dedups by name, so a re-login still updates rather than duplicates.
    let name = if email.is_empty() {
        "claude:account".to_string()
    } else {
        format!("claude:{email}")
    };

    Ok(AccountConfig {
        name,
        credential: AccountCredential::Oauth {
            account_uuid,
            access_token: tokens.access_token,
            // A fresh code exchange always carries a refresh token; `None`
            // (refresh-style response) degrades to empty.
            refresh_token: tokens.refresh_token.unwrap_or_default(),
            expires_at_ms: tokens.expires_at_ms,
            tier,
            // Login mints a brand-new token — that IS a refresh for the
            // dashboard's "refreshed ago" display.
            last_refresh_ms: Some(super::now_ms()),
        },
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
