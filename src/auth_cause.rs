//! Why an account was benched for authentication, and what fixes it.
//!
//! The pool used to keep only the HTTP status of an auth failure, so every
//! surface printed `! 400 auth X` for four different situations: a login that
//! reached its lifetime, a login another daemon rotated away, a grant the
//! user revoked, and an account the provider refused. Each needs a different
//! fix, and the operator had to read the daemon log to tell them apart.
//!
//! [`AuthCause`] is decided once, where the failure happens, from facts the
//! application owns: which path failed and the OAuth `error` code and
//! `error_description`. Provider text is matched, never echoed, so nothing a
//! token endpoint returns can reach a display through this type.

use serde::{Deserialize, Serialize};

/// The classified reason an account is excluded until an operator acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthCause {
    /// Token endpoint said `invalid_grant` / "refresh token expired": the
    /// login reached its lifetime. Claude sign-ins measured on a shared host
    /// (2026-11-12..13) died 28.7–29.9 days after the login that minted them.
    LoginExpired,
    /// `invalid_grant` / "refresh token not found or invalid": another holder
    /// of the same login (a second daemon, a copied config) refreshed first
    /// and the token rotated away from this daemon.
    LoginRotated,
    /// `invalid_grant` / "... revoked": the grant was revoked in the
    /// provider's settings, or rotated elsewhere (grok words rotation this way).
    LoginRevoked,
    /// Any other refresh rejection: a bare `invalid_grant`, or a 401 from the
    /// token endpoint without a recognised description.
    RefreshRejected,
    /// `invalid_client`: the provider refused llmux's OAuth client.
    ClientRejected,
    /// The provider rejected the OAuth access token — again after the
    /// refresh that should have fixed it, for the kinds llmux refreshes.
    TokenRejected,
    /// The provider rejected an API key (nothing to refresh).
    KeyRejected,
    /// The provider refused the account itself on a request (persistent
    /// account rejection, not a token problem).
    AccountRejected,
    /// The provider suspended this account's access (Google ToS on gemini).
    AccessSuspended,
    /// The usage endpoint answered 403 for this login.
    UsageForbidden,
    /// A persistent, non-auth provider error benched the account.
    ProviderError,
}

/// When and why the pool benched an account. Set together with the health
/// change and cleared by the same re-login that heals it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthFailure {
    pub cause: AuthCause,
    /// Epoch ms at which the account was benched.
    pub at_ms: u64,
}

impl AuthCause {
    /// Every cause, for exhaustive tests and docs.
    pub const ALL: [AuthCause; 11] = [
        AuthCause::LoginExpired,
        AuthCause::LoginRotated,
        AuthCause::LoginRevoked,
        AuthCause::RefreshRejected,
        AuthCause::ClientRejected,
        AuthCause::TokenRejected,
        AuthCause::KeyRejected,
        AuthCause::AccountRejected,
        AuthCause::AccessSuspended,
        AuthCause::UsageForbidden,
        AuthCause::ProviderError,
    ];

    /// Classify a refresh the token endpoint rejected permanently (a 401, or
    /// any body carrying `invalid_grant`). Only the OAuth `error` code and
    /// `error_description` are read; unknown text maps to
    /// [`AuthCause::RefreshRejected`] rather than being shown.
    pub fn from_refresh_rejection(body: &str) -> Self {
        let doc = serde_json::from_str::<serde_json::Value>(body).ok();
        let field = |name: &str| {
            doc.as_ref()
                .and_then(|d| d.get(name))
                .and_then(serde_json::Value::as_str)
                .map(str::to_ascii_lowercase)
        };
        let description = field("error_description").unwrap_or_default();
        // "Token has been expired or revoked." (Google) names both: the
        // provider did not say which, so neither is claimed.
        let expired = description.contains("expired");
        let revoked = description.contains("revoked");
        match field("error").as_deref() {
            Some("invalid_client") => AuthCause::ClientRejected,
            Some("invalid_grant") if expired && revoked => AuthCause::RefreshRejected,
            Some("invalid_grant") if expired => AuthCause::LoginExpired,
            Some("invalid_grant") if description.contains("not found or invalid") => {
                AuthCause::LoginRotated
            }
            Some("invalid_grant") if revoked => AuthCause::LoginRevoked,
            _ => AuthCause::RefreshRejected,
        }
    }

    /// Short label for the status column (`! 400 login expired`). At most 16
    /// characters so `! NNN <label>` fits the 20-column status cell.
    pub fn label(self) -> &'static str {
        match self {
            AuthCause::LoginExpired => "login expired",
            AuthCause::LoginRotated => "login rotated",
            AuthCause::LoginRevoked => "login revoked",
            AuthCause::RefreshRejected => "refresh rejected",
            AuthCause::ClientRejected => "client rejected",
            AuthCause::TokenRejected => "token rejected",
            AuthCause::KeyRejected => "key rejected",
            AuthCause::AccountRejected => "account rejected",
            AuthCause::AccessSuspended => "access suspended",
            AuthCause::UsageForbidden => "usage forbidden",
            AuthCause::ProviderError => "provider error",
        }
    }

    /// One sentence on what happened, in the operator's terms.
    pub fn why(self) -> &'static str {
        match self {
            AuthCause::LoginExpired => {
                "the provider ended this login: its refresh token reached its lifetime (Claude sign-ins last about 30 days)"
            }
            AuthCause::LoginRotated => {
                "another holder of this login refreshed first, so this daemon's refresh token was rotated away (a second daemon or a copied config?)"
            }
            AuthCause::LoginRevoked => {
                "the provider says this login was revoked: signed out or revoked in the account's settings, or rotated by another holder"
            }
            AuthCause::RefreshRejected => {
                "the provider refused to refresh this login and gave no recognised reason"
            }
            AuthCause::ClientRejected => {
                "the provider refused llmux's OAuth client, not this account"
            }
            AuthCause::TokenRejected => {
                "the provider rejected this account's access token, even after a refresh where one was possible"
            }
            AuthCause::KeyRejected => "the provider rejected this account's API key",
            AuthCause::AccountRejected => {
                "the provider refused this account on a request (billing, organization or plan problem)"
            }
            AuthCause::AccessSuspended => "the provider suspended this account's access",
            AuthCause::UsageForbidden => {
                "the usage endpoint answered 403: the login was revoked or the account lost access"
            }
            AuthCause::ProviderError => {
                "a persistent provider error (not an auth answer) benched this account"
            }
        }
    }

    /// How strong a verdict this cause is on the credential, for deciding
    /// whether a later failure of the SAME credential replaces it: a newer
    /// failure replaces the recorded one only when it is at least as strong.
    /// 0 — a downstream symptom (a request or usage read failing);
    /// 1 — the token endpoint refused without saying why;
    /// 2 — the token endpoint named what happened to the login or client;
    /// 3 — the provider refused or suspended the account itself.
    pub fn rank(self) -> u8 {
        match self {
            AuthCause::TokenRejected
            | AuthCause::KeyRejected
            | AuthCause::UsageForbidden
            | AuthCause::ProviderError => 0,
            AuthCause::RefreshRejected => 1,
            AuthCause::LoginExpired
            | AuthCause::LoginRotated
            | AuthCause::LoginRevoked
            | AuthCause::ClientRejected => 2,
            AuthCause::AccountRejected | AuthCause::AccessSuspended => 3,
        }
    }

    /// Whether signing in again is the fix (it is for every login-side cause).
    pub fn needs_relogin(self) -> bool {
        !matches!(
            self,
            AuthCause::ClientRejected | AuthCause::AccessSuspended | AuthCause::ProviderError
        )
    }

    /// The operator's next step for an account of credential `kind`
    /// (`oauth`, `codex`, `grok`, `gemini`, `openrouter`, `apikey`).
    pub fn fix(self, kind: &str) -> String {
        let login = login_command(kind);
        match self {
            AuthCause::LoginRotated => format!(
                "sign in again on the daemon's host: `{login}`; keep each login on one daemon only"
            ),
            AuthCause::ClientRejected => {
                "update llmux (`llmux update`), then sign in again if it persists".to_string()
            }
            AuthCause::AccountRejected => format!(
                "check the account's standing with the provider, then sign in again: `{login}`"
            ),
            AuthCause::AccessSuspended => format!(
                "resolve the suspension with the provider, then sign in again to re-enable the account (`{login}`), or remove it (`llmux remove <name>`)"
            ),
            AuthCause::KeyRejected => format!("add a valid key: `{login}`"),
            AuthCause::ProviderError => format!(
                "check the provider's status and the daemon log; sign in again to retry: `{login}`"
            ),
            AuthCause::LoginExpired
            | AuthCause::LoginRevoked
            | AuthCause::RefreshRejected
            | AuthCause::TokenRejected
            | AuthCause::UsageForbidden => {
                format!("sign in again on the daemon's host: `{login}`")
            }
        }
    }
}

/// The `llmux login` invocation that re-creates an account of `kind`.
pub fn login_command(kind: &str) -> &'static str {
    match kind {
        "codex" => "llmux login --codex",
        "grok" => "llmux login --grok",
        "gemini" => "llmux login --gemini",
        "openrouter" => "llmux login --openrouter",
        "apikey" => "llmux login --api",
        _ => "llmux login",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_rejections_classify_by_code_and_description() {
        let cases = [
            (
                r#"{"error":"invalid_grant","error_description":"Refresh token expired"}"#,
                AuthCause::LoginExpired,
            ),
            (
                r#"{"error":"invalid_grant","error_description":"Refresh token not found or invalid"}"#,
                AuthCause::LoginRotated,
            ),
            (
                r#"{"error":"invalid_grant","error_description":"Refresh token has been revoked"}"#,
                AuthCause::LoginRevoked,
            ),
            (
                r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
                AuthCause::RefreshRejected,
            ),
            (r#"{"error":"invalid_grant"}"#, AuthCause::RefreshRejected),
            (r#"{"error":"invalid_client"}"#, AuthCause::ClientRejected),
            ("<html>invalid_grant</html>", AuthCause::RefreshRejected),
            ("", AuthCause::RefreshRejected),
        ];
        for (body, want) in cases {
            assert_eq!(AuthCause::from_refresh_rejection(body), want, "{body}");
        }
    }

    #[test]
    fn labels_fit_the_status_cell_and_round_trip_as_snake_case() {
        for cause in AuthCause::ALL {
            assert!(cause.label().len() <= 16, "{}", cause.label());
            assert!(!cause.why().is_empty());
            let json = serde_json::to_value(cause).expect("serialize");
            assert_eq!(
                json,
                serde_json::Value::from(cause.label().replace(' ', "_"))
            );
            assert_eq!(
                serde_json::from_value::<AuthCause>(json).expect("deserialize"),
                cause
            );
        }
    }

    /// The web dashboard's Accounts view maps every cause id the daemon can
    /// emit; a cause missing there falls back to "Sign-in failed". Checked
    /// inside the `X.AUTH_CAUSE = { … };` literal only, so a mention
    /// elsewhere in the file (a comment) does not count. (The node test
    /// `webui/test/core-auth.test.mjs` runs the loaded map itself.)
    #[test]
    fn web_dashboard_maps_every_cause_id() {
        let web = include_str!("../webui/public/core.js");
        let start = web.find("X.AUTH_CAUSE = {").expect("AUTH_CAUSE map");
        let map = &web[start..start + web[start..].find("};").expect("map end")];
        for cause in AuthCause::ALL {
            let id = serde_json::to_value(cause).expect("serialize");
            let key = format!("{}: [", id.as_str().expect("string id"));
            assert!(map.contains(&key), "X.AUTH_CAUSE lacks {key}");
        }
    }

    /// An API key was never refreshed: its rejection must not be explained
    /// as a failed refresh, and its fix is a new key.
    #[test]
    fn api_key_rejection_is_its_own_cause() {
        assert!(!AuthCause::KeyRejected.why().contains("refresh"));
        assert_eq!(
            AuthCause::KeyRejected.fix("apikey"),
            "add a valid key: `llmux login --api`"
        );
    }

    #[test]
    fn fix_names_the_login_command_for_the_account_kind() {
        assert_eq!(
            AuthCause::LoginExpired.fix("oauth"),
            "sign in again on the daemon's host: `llmux login`"
        );
        assert!(AuthCause::LoginExpired
            .fix("codex")
            .contains("llmux login --codex"));
        assert!(AuthCause::ClientRejected
            .fix("oauth")
            .contains("llmux update"));
        assert!(!AuthCause::ClientRejected.needs_relogin());
        assert!(AuthCause::LoginExpired.needs_relogin());
    }
}
