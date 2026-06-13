//! `teamagent login [--api]` — add an account.

use crate::auth::{oauth, profile};
use crate::config::{AccountConfig, AccountCredential, Config, Upsert};

use super::{prompt_line, CliError, LoginArgs};

/// OAuth path: PKCE browser flow → profile fetch (accountUuid, email) →
/// upsert into config by `account_uuid` (FR2 dedup).
/// `--api` path: prompt for an API key, store as an apikey account.
pub async fn run(args: LoginArgs) -> Result<(), CliError> {
    if args.api {
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
    let tokens = oauth::login_interactive(&client).await?;

    // Profile fetch enriches uuid/name/tier; a failure degrades to an
    // unenriched account rather than losing the freshly minted tokens.
    let fetched = profile::fetch_profile(&client, &config.upstream, &tokens.access_token).await;
    let (account_uuid, name, tier) = match fetched {
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

    let mut final_name = String::new();
    let mut outcome = Upsert::Added;
    crate::config::update(|config: &mut Config| {
        let resolved_name = if name.is_empty() {
            let n = config
                .accounts
                .iter()
                .filter(|a| a.name.starts_with("account-"))
                .count()
                + 1;
            format!("account-{n}")
        } else {
            name.clone()
        };
        final_name = resolved_name.clone();
        outcome = config.upsert_account(AccountConfig {
            name: resolved_name,
            credential: AccountCredential::Oauth {
                account_uuid: account_uuid.clone(),
                access_token: tokens.access_token.clone(),
                // A fresh code exchange always carries a refresh token;
                // `None` (refresh-style response) degrades to empty.
                refresh_token: tokens.refresh_token.clone().unwrap_or_default(),
                expires_at_ms: tokens.expires_at_ms,
                tier: tier.clone(),
                // Login mints a brand-new token — that IS a refresh for
                // the dashboard's "refreshed ago" display.
                last_refresh_ms: Some(super::now_ms()),
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
