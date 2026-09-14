//! Re-login acceptance (`docs/keys-history/spec.md` §L): an account the pool
//! benched as `AuthFailed` comes back through `POST /llmux/inject-account` —
//! the endpoint the TUI `n`-key and the attach-mode client both land on.
//!
//! Driven through the REAL authenticated endpoint on a real socket, and
//! asserted on BOTH sides of the write: the config file on disk (one row, new
//! token, no duplicate) and the live pool (new credential, health restored).
//!
//! Isolation: every test owns its proxy (port 0) and a tempdir config —
//! nothing touches the real `~/.config`. No upstream is contacted: the
//! injected tokens never expire inside a test's lifetime, so the background
//! refresher stays idle, and no client request is driven.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use llmux::config::{self, AccountConfig, AccountCredential, Config};
use llmux::proxy::server::{serve, AppState};
use llmux::scheduler::{AccountId, AccountPool};

/// Self-cleaning unique temp dir (no tempfile dev-dependency).
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "llmux-relogin-{}-{}",
            std::process::id(),
            ulid::Ulid::new()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Beyond the background refresh window, so the server never refreshes a
/// token behind the test's back.
fn far_future_ms() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64;
    now + 24 * 3_600 * 1_000
}

/// Same name and same upstream uuid, caller-chosen access token — the shape a
/// provider login returns for an account that already exists.
fn oauth_account(name: &str, access_token: &str) -> AccountConfig {
    AccountConfig {
        name: name.to_string(),
        credential: AccountCredential::Oauth {
            account_uuid: format!("uuid-{name}"),
            access_token: access_token.to_string(),
            refresh_token: format!("rt-{name}"),
            expires_at_ms: far_future_ms(),
            tier: None,
            last_refresh_ms: None,
        },
    }
}

/// An account whose stable upstream identity is `uuid`, independent of its
/// local `name` — the shape a profile-email rename produces.
fn oauth_account_with_uuid(name: &str, uuid: &str, access_token: &str) -> AccountConfig {
    let mut account = oauth_account(name, access_token);
    if let AccountCredential::Oauth { account_uuid, .. } = &mut account.credential {
        *account_uuid = uuid.to_string();
    }
    account
}

/// The admin credential the test proxy is seeded with: control-plane routes
/// require one even on loopback.
const ADMIN_KEY: &str = "lm-relogin-admin";

struct Proxy {
    addr: SocketAddr,
    pool: AccountPool,
    config_path: PathBuf,
    _tmp: TempDir,
}

impl Proxy {
    async fn spawn(accounts: Vec<AccountConfig>) -> Self {
        let mut config = Config {
            // Never contacted; a reserved-port URL makes an accidental
            // outbound request fail loudly instead of reaching anything real.
            upstream: "http://127.0.0.1:1".to_string(),
            accounts,
            ..Default::default()
        };
        config.proxy.api_key = Some(ADMIN_KEY.into());
        config.proxy.port = 0; // OS-assigned; `serve` reports it via `ready`
        config.proxy.idle_probe.enabled = false;

        let tmp = TempDir::new();
        let config_path = tmp.path().join("llmux.json");
        config::save_path(&config_path, &config).expect("seed config");

        let pool = AccountPool::new(&config.accounts);
        let mut state = AppState::new(config, pool.clone(), None, None).expect("app state");
        // Every write must land in the tempdir, never the user's real config
        // / state files.
        state.config_path = Some(config_path.clone());
        state.activity_log_path = Some(tmp.path().join("activity.jsonl"));
        state.raw_io_path = Some(tmp.path().join("raw-io.jsonl"));

        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(serve(state, Some(ready_tx)));
        let addr = ready_rx.await.expect("proxy ready");
        Self {
            addr,
            pool,
            config_path,
            _tmp: tmp,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.addr.port(), path)
    }

    /// `POST /llmux/inject-account` with the admin credential, body = the
    /// `type`-tagged account the client minted.
    async fn inject(&self, account: &AccountConfig) -> (u16, serde_json::Value) {
        let response = reqwest::Client::new()
            .post(self.url("/llmux/inject-account"))
            .header("x-api-key", ADMIN_KEY)
            .json(&serde_json::to_value(account).expect("serialize account"))
            .send()
            .await
            .expect("inject reachable");
        let status = response.status().as_u16();
        (status, response.json().await.expect("inject json"))
    }

    fn access_token(&self, account: &str) -> String {
        match self.pool.credential(&AccountId(account.into())) {
            Some(AccountCredential::Oauth { access_token, .. }) => access_token,
            other => panic!("unexpected pool credential {other:?}"),
        }
    }

    fn healthy(&self, account: &str) -> bool {
        self.pool
            .snapshot()
            .accounts
            .iter()
            .find(|a| a.id == AccountId(account.into()))
            .map(|a| a.healthy)
            .expect("account in pool")
    }
}

/// §L steps 3–5: re-login of an account that died on auth. ONE config row is
/// replaced (never duplicated), the live pool serves the new token, and the
/// account is selectable again with no restart.
#[tokio::test]
async fn relogin_replaces_credential_and_revives_the_failed_account() {
    let proxy = Proxy::spawn(vec![
        oauth_account("a", "at-a-old"),
        oauth_account("b", "at-b"),
    ])
    .await;
    proxy.pool.record_auth_failure(&AccountId("a".into()));
    assert!(!proxy.healthy("a"), "precondition: a is benched on auth");

    let (status, body) = proxy.inject(&oauth_account("a", "at-a-relogin")).await;

    assert_eq!(status, 200);
    assert_eq!(body["ok"], true);
    assert_eq!(body["name"], "a");
    assert_eq!(body["added"], false, "a re-login updates, never adds");

    // Config side: the row was replaced in place.
    let config = config::load_path(&proxy.config_path).expect("reload config");
    assert_eq!(config.accounts.len(), 2, "no duplicate account row");
    let stored = config
        .accounts
        .iter()
        .find(|a| a.name == "a")
        .expect("account a persisted");
    match &stored.credential {
        AccountCredential::Oauth { access_token, .. } => assert_eq!(access_token, "at-a-relogin"),
        other => panic!("unexpected stored credential {other:?}"),
    }

    // Pool side: same roster, new token, health restored — no restart.
    assert_eq!(proxy.pool.snapshot().accounts.len(), 2);
    assert_eq!(proxy.access_token("a"), "at-a-relogin");
    assert!(
        proxy.healthy("a"),
        "the re-login ends the auth failure in the live pool"
    );
    assert!(proxy.healthy("b"), "the untouched account is unaffected");
}

/// §L step 4: re-injecting the SAME credential is not a re-login — it carries
/// no new evidence, so the auth failure must survive it.
#[tokio::test]
async fn reinjecting_an_unchanged_credential_keeps_the_auth_failure() {
    let unchanged = oauth_account("a", "at-a");
    let proxy = Proxy::spawn(vec![unchanged.clone()]).await;
    proxy.pool.record_auth_failure(&AccountId("a".into()));

    let (status, body) = proxy.inject(&unchanged).await;

    assert_eq!(status, 200);
    assert_eq!(body["added"], false);
    assert_eq!(proxy.pool.snapshot().accounts.len(), 1, "no duplicate");
    assert!(
        !proxy.healthy("a"),
        "a byte-identical credential does not resurrect a failed account"
    );
}

/// `docs/keys-history/relogin-trace.md` B3 — the DISK half of the stale-refresh
/// race. The pool CAS and the config write are necessarily two steps: a
/// re-login that lands between them must not be overwritten by a refresh that
/// started from the credential it retired, or the next restart loses it.
#[test]
fn a_stale_refresh_is_refused_by_the_config_row_it_would_overwrite() {
    let mut config = Config {
        accounts: vec![oauth_account("a", "at-a-old")],
        ..Default::default()
    };
    let retired = config.accounts[0].credential.clone();

    // The re-login wins the race to disk.
    config.upsert_account(oauth_account("a", "at-a-relogin"));

    // The refresh that started from `retired` lands afterwards.
    let applied = config.update_oauth_tokens_if(
        "uuid-a",
        |stored| {
            llmux::scheduler::credential_digest(stored)
                == llmux::scheduler::credential_digest(&retired)
        },
        "at-from-retired-refresh",
        Some("rt-from-retired-refresh"),
        42,
        41,
    );

    assert!(!applied, "the stale refresh is refused");
    match &config.accounts[0].credential {
        AccountCredential::Oauth { access_token, .. } => {
            assert_eq!(access_token, "at-a-relogin", "the re-login row survives")
        }
        other => panic!("unexpected credential {other:?}"),
    }
}

/// The same guard still lets a NORMAL refresh through: nothing raced it, so
/// the stored credential is exactly the one the refresh started from.
#[test]
fn an_unraced_refresh_still_persists() {
    let mut config = Config {
        accounts: vec![oauth_account("a", "at-a")],
        ..Default::default()
    };
    let started_from = config.accounts[0].credential.clone();

    let applied = config.update_oauth_tokens_if(
        "uuid-a",
        |stored| {
            llmux::scheduler::credential_digest(stored)
                == llmux::scheduler::credential_digest(&started_from)
        },
        "at-a-refreshed",
        None,
        42,
        41,
    );

    assert!(applied);
    match &config.accounts[0].credential {
        AccountCredential::Oauth {
            access_token,
            refresh_token,
            last_refresh_ms,
            ..
        } => {
            assert_eq!(access_token, "at-a-refreshed");
            assert_eq!(
                refresh_token, "rt-a",
                "None preserves the stored refresh token"
            );
            assert_eq!(*last_refresh_ms, Some(41));
        }
        other => panic!("unexpected credential {other:?}"),
    }
}

/// `docs/keys-history/relogin-trace.md` B6: a re-login whose profile email
/// changed matches by STABLE UUID. Replacing the whole row would rename the
/// account — and `paused_accounts`, `account_limits` and every scheduler
/// per-account state are keyed by NAME, so the rename silently un-pauses the
/// account and drops its ceilings. The established name wins instead.
#[test]
fn a_uuid_matched_relogin_keeps_the_established_name_and_its_user_state() {
    let mut config = Config {
        accounts: vec![oauth_account_with_uuid(
            "claude:old@x.com",
            "uuid-a",
            "at-a-old",
        )],
        ..Default::default()
    };
    config.paused_accounts.insert("claude:old@x.com".into());
    config.account_limits.insert(
        "claude:old@x.com".into(),
        llmux::config::AccountLimits {
            five_hour_max: Some(0.5),
            ..Default::default()
        },
    );

    let outcome = config.upsert_account(oauth_account_with_uuid(
        "claude:new@x.com",
        "uuid-a",
        "at-a-relogin",
    ));

    assert_eq!(outcome, llmux::config::Upsert::Updated);
    assert_eq!(config.accounts.len(), 1, "no duplicate");
    assert_eq!(
        config.accounts[0].name, "claude:old@x.com",
        "the established name survives a uuid-matched re-login"
    );
    match &config.accounts[0].credential {
        AccountCredential::Oauth { access_token, .. } => {
            assert_eq!(access_token, "at-a-relogin", "the credential IS replaced")
        }
        other => panic!("unexpected credential {other:?}"),
    }
    assert!(
        config.paused_accounts.contains(&config.accounts[0].name),
        "the operator pause still points at a real account"
    );
    assert!(
        config.account_limits.contains_key(&config.accounts[0].name),
        "the per-account ceilings still point at a real account"
    );
}

/// The name-matched half is unchanged: with no stable identity the NAME is the
/// identity, so the caller's entry replaces it wholesale.
#[test]
fn a_name_matched_upsert_still_replaces_the_row() {
    let mut config = Config {
        accounts: vec![AccountConfig {
            name: "api-1".into(),
            credential: AccountCredential::Apikey {
                api_key: "sk-ant-old".into(),
            },
        }],
        ..Default::default()
    };

    let outcome = config.upsert_account(AccountConfig {
        name: "api-1".into(),
        credential: AccountCredential::Apikey {
            api_key: "sk-ant-new".into(),
        },
    });

    assert_eq!(outcome, llmux::config::Upsert::Updated);
    assert_eq!(config.accounts.len(), 1);
    match &config.accounts[0].credential {
        AccountCredential::Apikey { api_key } => assert_eq!(api_key, "sk-ant-new"),
        other => panic!("unexpected credential {other:?}"),
    }
}

/// B6 through the real endpoint: the response must report the name the ROSTER
/// ended up with, not the label the caller sent — otherwise the dashboard
/// shows an account that exists nowhere, and the live pool keeps its state
/// under the established name.
#[tokio::test]
async fn relogin_under_a_new_label_reports_the_established_name() {
    let proxy = Proxy::spawn(vec![oauth_account_with_uuid(
        "claude:old@x.com",
        "uuid-a",
        "at-a-old",
    )])
    .await;
    proxy
        .pool
        .record_auth_failure(&AccountId("claude:old@x.com".into()));

    let (status, body) = proxy
        .inject(&oauth_account_with_uuid(
            "claude:new@x.com",
            "uuid-a",
            "at-a-relogin",
        ))
        .await;

    assert_eq!(status, 200);
    assert_eq!(body["added"], false);
    assert_eq!(
        body["name"], "claude:old@x.com",
        "the response reports the resolved roster name"
    );

    let config = config::load_path(&proxy.config_path).expect("reload config");
    assert_eq!(config.accounts.len(), 1, "no duplicate account row");
    assert_eq!(config.accounts[0].name, "claude:old@x.com");

    let snapshot = proxy.pool.snapshot();
    assert_eq!(snapshot.accounts.len(), 1, "no duplicate pool entry");
    assert_eq!(
        snapshot.accounts[0].id,
        AccountId("claude:old@x.com".into())
    );
    assert_eq!(proxy.access_token("claude:old@x.com"), "at-a-relogin");
    assert!(
        proxy.healthy("claude:old@x.com"),
        "the re-login heals the auth failure under the established name"
    );
}
