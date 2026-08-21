//! `llmux login [--api | --codex | --grok | --openrouter]` — add an account.

use crate::auth::{codex, oauth, openrouter, profile, AuthError};
use crate::config::{AccountConfig, AccountCredential, Config, Upsert};

use super::{prompt_line, CliError, LoginArgs};

/// OAuth path: PKCE browser flow → profile fetch (accountUuid, email) →
/// upsert into config by `account_uuid` (FR2 dedup).
/// `--api` path: prompt for an API key, store as an apikey account.
/// `--codex` path: ChatGPT OAuth browser flow → upsert a Codex account.
/// `--grok` path: xAI device-code flow → upsert a Grok account.
/// `--openrouter` path: OpenRouter PKCE browser flow (or `--paste`) →
/// upsert an `or:<label>` account.
pub async fn run(args: LoginArgs) -> Result<(), CliError> {
    if args.openrouter {
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
        if label.is_empty() {
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
    let label = openrouter::fetch_key_label(&client, openrouter::KEY_INFO_URL, &api_key)
        .await
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
