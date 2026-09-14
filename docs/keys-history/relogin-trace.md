# Re-login trace amendment (spec §L)

Amends [`spec.md`](spec.md) §L with the code-level happy path, the concurrency
breaks a review found on it, and the fix for each. `spec.md` itself is owned by
another worker and is NOT edited here.

§L stated the roster hop only ("changed credentials restore AuthFailed to
Healthy"). That is necessary but not sufficient: a re-login lands on a LIVE
daemon, so it races three background writers that all carry a credential the
re-login just retired. Each of them could re-bench or overwrite the new
credential — in memory or on disk — after the re-login completed.

## Happy path (code-level hops)

| # | Hop | Where |
|---|---|---|
| 0 | Browser PKCE flow → tokens | `src/auth/oauth.rs` via `src/cli/login.rs:110` |
| 1 | Profile fetch → `(account_uuid, email, tier)` = the account's IDENTITY | `src/cli/login.rs:112` |
| 2 | Identity → `AccountConfig { name: claude:<email>, credential }` | `src/cli/login.rs:126-158` |
| 3 | Local TUI `n` / `POST /llmux/inject-account` → `AppState::inject_account` | `src/proxy/server.rs:833` |
| 4 | Read-merge-write: `config::update_path` closure → `Config::upsert_account` (uuid-first dedup) | `src/config/schema.rs:1017` |
| 5 | `AppState::apply_roster` → `AccountPool::reload_accounts` | `src/proxy/server.rs:881`, `:897` |
| 6 | Survivor with a CHANGED `credential_digest` → new generation + `AuthFailed` → `Healthy` | `src/scheduler/mod.rs:1471` |
| 7 | Next `evaluate`/`lease_for` serves the new credential; the config file is the SSOT on restart | `src/scheduler/mod.rs:1141` |

## Breaks found on that path (and the fix for each)

**B1 — stale 401 re-benches the new credential.** A request leased credential
`C_old`, the operator re-logged in while it was in flight, and the 401 that
`C_old` earned reached `state.pool.record_auth_failure(&account)`
(`src/proxy/forward.rs:1794`, and the same unconditional call at `:1511`
persistent-error and `:1325` refresh-permanent) — benching an account whose
live credential is `C_new` and was `Healthy`. The user-visible symptom is the
one §L set out to kill: re-login "does nothing", the account stays benched.
*Fix:* the lease now carries the `AccountFingerprint` captured in the SAME
write lock that pinned the credential (`AccountLease::fingerprint`), and the
call site uses `AccountPool::record_auth_failure_if(account, expected)`, which
re-checks identity+generation+digest under the write lock (the `record_usage_if`
pattern from `.prd/16`). A fingerprint captured separately from the credential
would reintroduce the race, so it is never read on its own.

**B2 — stale refresh overwrites the new credential in memory.** A refresh
started from `C_old` completes after the re-login and calls
`state.pool.update_credential` unconditionally (`src/proxy/forward.rs:2140`),
replacing `C_new` with a token minted from the RETIRED refresh token.
*Fix:* `update_credential_if(account, expected, fresh)` guarded by the same
fingerprint, and `refresh_credential` takes the expected fingerprint from its
caller. When the CAS refuses, the outcome is `RefreshOutcome::Superseded` — a
variant every caller treats as "do nothing, the pool already moved on".

**B3 — stale refresh overwrites the new credential on DISK.** Even with B2
fixed, `persist_tokens` wrote by identity alone (`src/proxy/forward.rs:2145` →
`Config::update_oauth_tokens`), so the config row the re-login had just written
was overwritten by the old refresh's tokens — the pool was right and the file
was wrong, and the next restart lost the re-login. The window is real because
the pool CAS and the disk write are necessarily separate operations.
*Fix:* `Config::update_oauth_tokens_if(ident, expected_digest, …)` compares the
digest of the credential ON DISK inside the `config::update_path` closure and
refuses when it no longer matches the credential the refresh started from. The
comparison therefore happens under the same read-merge-write that does the
write; it cannot be lost between check and write.

**B4 — stale usage poll.** `UsagePoller::poll_account` read the credential,
awaited the fetch, then recorded usage / an auth failure against whatever the
account had become (`src/scheduler/usage.rs:452-467`). A 403 earned by `C_old`
benched the re-logged-in account.
*Fix:* one atomic `credential_with_fingerprint` capture, then
`record_usage_if` / `record_auth_failure_if` with it.

**B5 — background refresh pass.** `background_refresh_pass`
(`src/proxy/server.rs:1339`) benched on `RefreshOutcome::Permanent`
unconditionally — same shape as B1, reached without any request.
*Fix:* capture `(credential, fingerprint)` together and pass the fingerprint
into `refresh_credential`; bench through `record_auth_failure_if`.

**B6 — uuid-matched rename drops user state.** `Config::upsert_account`
replaced the whole entry on a stable-uuid match, so a re-login whose profile
email changed also RENAMED the account — while `paused_accounts`,
`account_limits`, the scheduler's per-name pool state (quota windows,
cooldowns, in-flight leases, sticky `current`) are all keyed by NAME. The
rename silently resumed a paused account, dropped its per-account ceilings, and
made the pool treat it as a brand-new account.
*Fix (intentional, minimal):* **when the match is by stable identity (uuid),
the ESTABLISHED name wins** — the credential is replaced in place, the name is
not. Only a name-matched upsert (no uuid) keeps the caller's name, where it is
the identity by definition. The alternative — migrating `paused_accounts`,
`account_limits`, pool state and history keys to the new name — is a
multi-owner identity migration; it is deliberately NOT done here, and the
account name stays a stable local label rather than a mirror of the profile
email. `Config::upsert_account`'s old "a re-login may rename the account to its
profile email" contract is hereby retired (its test in `src/config/mod.rs` is
updated in the same change).

**B7 — unidentified OAuth login.** A failed profile fetch degraded to
`account_uuid: ""` + name `claude:account` (`src/cli/login.rs:113-135`), an
account with NO stable identity: `AccountCredential::account_uuid()` returns
`None` for it (`src/config/schema.rs:980`), so dedup fell back to name and the
next such login either collided with it or piled up another
`claude:account-N` — the duplicate §L step 5 forbids.
*Fix:* fail visibly. `oauth_login_to_account` returns an error when the profile
fetch fails or yields an empty `account_uuid`; nothing is persisted. Identity is
NEVER inferred from the token itself (a token prefix is not an identity, and it
rotates). When the profile is identified but carries no email, the name falls
back to the uuid — a real stable identity — not to a shared placeholder.

## What must still be true after the fixes

- A re-login while requests are in flight: leases keep serving `C_old` to
  completion (never yanked), their results never write back health or
  credentials, and the account stays `Healthy` on `C_new`.
- An unchanged credential re-applied (pause toggle, `import`, re-inject of the
  same bytes) never resurrects a failed account.
- Quota windows, operator pause, cooldowns and per-account limits survive a
  re-login, in memory and in the config file.
- A genuine 401 from the CURRENT credential still benches the account on the
  second try — the guards refuse stale results, not real failures.
