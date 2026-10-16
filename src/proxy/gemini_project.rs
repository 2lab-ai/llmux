//! Lazy Cloud Code project resolution for gemini accounts
//! (docs/gemini/spec.md §R5): an account imported while its access token was
//! expired has no project, and every Cloud Code call — the model stream AND
//! the quota read — is addressed by one.
//!
//! ONE implementation behind every path that needs the project: the request
//! path (`forward`, after the token refresh), the explicit usage refresh
//! (`usage_controls`) and the background usage poller
//! (`scheduler::usage`). The only upstream traffic is
//! [`crate::auth::gemini::resolve_project`] (`loadCodeAssist`, and
//! `onboardUser` when the account has no project yet).

use std::path::Path;

use crate::auth::AuthError;
use crate::config::AccountCredential;
use crate::scheduler::{credential_digest, AccountFingerprint, AccountId, AccountPool};

/// Everything a lookup needs besides the account: the HTTP client, the
/// CONFIGURED Cloud Code base + Antigravity identity, the pool the project is
/// applied to and the config file it is persisted in (`None` = in memory
/// only).
pub(crate) struct ProjectLookup<'a> {
    pub client: &'a reqwest::Client,
    pub upstream: &'a str,
    pub user_agent: &'a str,
    pub pool: &'a AccountPool,
    pub config_path: Option<&'a Path>,
}

/// Outcome of [`ensure_project`].
pub(crate) enum GeminiProject {
    /// Not a gemini credential, or its project is already known.
    Ready,
    /// Project resolved, applied to the pool and persisted: use this
    /// credential. `fingerprint` is the pool's fingerprint OF it — the write
    /// changed the credential digest, so a guarded write that follows (a
    /// usage reading) must carry this one, not the pre-lookup capture.
    Resolved {
        credential: AccountCredential,
        fingerprint: AccountFingerprint,
    },
    /// The pool no longer holds the credential the lookup started from (a
    /// re-login); nothing was written.
    Superseded,
    /// `loadCodeAssist` / `onboardUser` failed (already logged); nothing was
    /// written.
    Failed(AuthError),
}

/// Whether `credential` is a gemini account still waiting for its project
/// AND holding an access token the request path would use as-is (outside
/// its refresh-ahead margin). A usage read never refreshes a token, so an
/// expiring one waits for the normal refresh and the next read.
pub(crate) fn awaits_lookup(credential: &AccountCredential) -> bool {
    matches!(
        credential,
        AccountCredential::Gemini { project_id, expires_at_ms, .. }
            if project_id.trim().is_empty() && !super::forward::expiring_soon(*expires_at_ms)
    )
}

/// Resolve a gemini credential's missing Cloud Code project
/// (`auth::gemini::resolve_project`), apply it to the pool and persist it.
///
/// The pool write is fingerprint-guarded against the credential captured
/// right before it, and only when that IS the credential in hand — not a
/// lease's fingerprint, which a refresh earlier in the same request has
/// already made stale. The disk write is guarded by the same digest
/// ([`persist_project`]).
pub(crate) async fn ensure_project(
    lookup: &ProjectLookup<'_>,
    account: &AccountId,
    credential: &AccountCredential,
) -> GeminiProject {
    let AccountCredential::Gemini {
        email,
        access_token,
        project_id,
        ..
    } = credential
    else {
        return GeminiProject::Ready;
    };
    if !project_id.trim().is_empty() {
        return GeminiProject::Ready;
    }
    let project = match crate::auth::gemini::resolve_project(
        lookup.client,
        lookup.upstream,
        lookup.user_agent,
        access_token,
    )
    .await
    {
        Ok(project) => project,
        Err(err) => {
            tracing::warn!(account = %account, reason = %failure_reason(&err),
                "gemini project lookup failed");
            return GeminiProject::Failed(err);
        }
    };
    let Some((current, expected)) = lookup.pool.credential_with_fingerprint(account) else {
        return GeminiProject::Superseded;
    };
    if current != *credential {
        return GeminiProject::Superseded;
    }
    let mut fresh = credential.clone();
    if let AccountCredential::Gemini { project_id, .. } = &mut fresh {
        *project_id = project.clone();
    }
    if !lookup
        .pool
        .update_credential_if(account, &expected, fresh.clone())
    {
        return GeminiProject::Superseded;
    }
    tracing::info!(account = %account, project = %project, "gemini project resolved");
    let ident = if email.is_empty() {
        account.0.clone()
    } else {
        email.clone()
    };
    persist_project(lookup.config_path, ident, &expected, &project).await;
    // `update_credential` keeps the generation and the identity: only the
    // digest moved.
    let fingerprint = AccountFingerprint {
        digest: credential_digest(&fresh),
        ..expected
    };
    GeminiProject::Resolved {
        credential: fresh,
        fingerprint,
    }
}

/// The request path's operator message for a failed lookup (the 502 body):
/// the HTTP status when there is one, else the sanitized error text.
pub(crate) fn failure_message(err: &AuthError) -> String {
    format!(
        "gemini project lookup (loadCodeAssist) failed: {}",
        failure_reason(err)
    )
}

/// The usage-control message for a failed lookup — the status phrase or a
/// fixed phrase per error class, never the error text (a transport error's
/// text carries the URL; the envelope carries no URL, body or credential).
pub(crate) fn usage_failure_message(err: &AuthError) -> String {
    let reason = match (err.http_status(), err) {
        (Some(status), _) => status.to_string(),
        (None, AuthError::GeminiAuth(phrase)) => (*phrase).to_string(),
        (None, AuthError::Network(_)) => "upstream request failed".to_string(),
        (None, _) => "upstream response was not readable".to_string(),
    };
    format!("gemini project lookup (loadCodeAssist) failed: {reason}")
}

fn failure_reason(err: &AuthError) -> String {
    match err.http_status() {
        Some(status) => status.to_string(),
        None => super::forward::sanitize_detail(&err.to_string()),
    }
}

/// Persist a lazily resolved gemini project with read-merge-write semantics,
/// guarded like the token persist: written only while the stored row is
/// still the credential the lookup started from (`expected`), so a re-login
/// landing in between is never overwritten. Failure is logged, never fatal —
/// the pool already holds the project; the next restart resolves it again.
async fn persist_project(
    config_path: Option<&Path>,
    ident: String,
    expected: &AccountFingerprint,
    project: &str,
) {
    let Some(path) = config_path.map(Path::to_path_buf) else {
        return;
    };
    let project = project.to_string();
    let expected_digest = expected.digest.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut written = false;
        let merged = crate::config::update_path(&path, |config| {
            let idx = config
                .accounts
                .iter()
                .position(|a| a.credential.account_uuid() == Some(ident.as_str()))
                .or_else(|| config.accounts.iter().position(|a| a.name == ident));
            let Some(row) = idx.map(|idx| &mut config.accounts[idx].credential) else {
                return;
            };
            if credential_digest(row) != expected_digest {
                return;
            }
            if let AccountCredential::Gemini { project_id, .. } = row {
                *project_id = project;
                written = true;
            }
        });
        (merged, written)
    })
    .await;
    match result {
        Ok((Ok(_), true)) => {}
        Ok((Ok(_), false)) => tracing::info!(
            "gemini project not persisted: the stored credential moved on (re-login)"
        ),
        Ok((Err(err), _)) => tracing::warn!(error = %err, "failed to persist gemini project"),
        Err(err) => tracing::warn!(error = %err, "gemini project persistence task failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AccountConfig, Config};
    use std::sync::{Arc, Mutex};

    /// Self-cleaning unique temp dir (no tempfile dev-dependency).
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "llmux-gemini-project-test-{}-{}",
                std::process::id(),
                ulid::Ulid::new()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }

    fn gemini(token: &str, project: &str, expires_at_ms: u64) -> AccountCredential {
        AccountCredential::Gemini {
            email: "m@example.com".into(),
            access_token: token.into(),
            refresh_token: format!("rt-{token}"),
            expires_at_ms,
            project_id: project.into(),
            last_refresh_ms: None,
        }
    }

    fn project_of(credential: &AccountCredential) -> String {
        match credential {
            AccountCredential::Gemini { project_id, .. } => project_id.clone(),
            other => panic!("expected a gemini credential, got {other:?}"),
        }
    }

    /// A Cloud Code stand-in answering `loadCodeAssist` with a project,
    /// counting the lookups.
    async fn spawn_lookup_mock() -> (String, Arc<Mutex<usize>>) {
        let hits: Arc<Mutex<usize>> = Arc::default();
        let counter = hits.clone();
        let app = axum::Router::new().fallback(move || {
            let counter = counter.clone();
            async move {
                *counter.lock().unwrap() += 1;
                r#"{"cloudaicompanionProject":"proj-lazy"}"#
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://127.0.0.1:{}", addr.port()), hits)
    }

    #[test]
    fn awaits_lookup_needs_an_empty_project_and_a_usable_token() {
        let hour = now_ms() + 3_600_000;
        assert!(awaits_lookup(&gemini("at", "", hour)));
        assert!(!awaits_lookup(&gemini("at", "proj", hour)), "already known");
        assert!(!awaits_lookup(&gemini("at", "", 0)), "expired");
        assert!(
            !awaits_lookup(&gemini("at", "", now_ms() + 60_000)),
            "inside the refresh-ahead margin: the normal refresh goes first"
        );
        assert!(!awaits_lookup(&AccountCredential::Apikey {
            api_key: "sk".into()
        }));
    }

    /// The pool still holds the credential the lookup started from, but the
    /// row ON DISK moved on (a re-login persisted, its pool reload not yet
    /// applied): the pool takes the project, the disk row is never
    /// overwritten.
    #[tokio::test]
    async fn persist_is_guarded_by_the_stored_row() {
        let (upstream, hits) = spawn_lookup_mock().await;
        let original = gemini("at-old", "", now_ms() + 3_600_000);
        let relogin = gemini("at-relogin", "", now_ms() + 3_600_000);
        let dir = TempDir::new();
        let path = dir.0.join("llmux.json");
        let on_disk = Config {
            accounts: vec![AccountConfig {
                name: "gemini:m".into(),
                credential: relogin.clone(),
            }],
            ..Default::default()
        };
        crate::config::save_path(&path, &on_disk).expect("seed config");
        let pool = AccountPool::new(&[AccountConfig {
            name: "gemini:m".into(),
            credential: original.clone(),
        }]);
        let client = reqwest::Client::new();
        let lookup = ProjectLookup {
            client: &client,
            upstream: &upstream,
            user_agent: "antigravity/test",
            pool: &pool,
            config_path: Some(&path),
        };
        let id = AccountId("gemini:m".into());

        let outcome = ensure_project(&lookup, &id, &original).await;

        let GeminiProject::Resolved {
            credential,
            fingerprint,
        } = outcome
        else {
            panic!("the pool still held the looked-up credential");
        };
        assert_eq!(project_of(&credential), "proj-lazy");
        assert_eq!(
            pool.fingerprint(&id),
            Some(fingerprint),
            "the returned fingerprint is the pool's, after the write"
        );
        assert_eq!(*hits.lock().unwrap(), 1);
        let stored = crate::config::load_path(&path).expect("config");
        assert_eq!(
            stored.accounts[0].credential, relogin,
            "the re-login's row is never overwritten"
        );
    }
}
