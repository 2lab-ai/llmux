//! Daemon usage-control service (`.prd/16-codex-usage-controls.md`): explicit
//! usage refresh, reset-credit listing, deliberate redemption, and the
//! refresh-before-commit manual switch.
//!
//! ONE implementation behind both surfaces — the `/llmux/*` handlers and the
//! in-process TUI call the [`AppState`] methods below, so local and attach
//! modes cannot drift.
//!
//! The load-bearing invariants (each is a test in `tests/usage_controls.rs`):
//!
//! - **Unknown ≠ zero.** A failed or malformed read never clears a window and
//!   never turns an absent counter into `0`.
//! - **Identity, not name.** Every observation is captured with the account's
//!   credential fingerprint and applied only if the pool still carries it — a
//!   removed/replaced account cannot receive a predecessor's result.
//! - **The POST is irreversible.** A non-secret pending receipt is persisted
//!   BEFORE the redemption; an uncertain outcome keeps it, so only the ORIGINAL
//!   request id can retry and a restart cannot spend a fresh key.
//! - **The daemon never mints an idempotency key.** The client owns it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::auth::codex_usage::{self, CodexUsage, CodexUsageError, ResetCredit, ResetCredits};
use crate::config::{AccountCredential, Config};
use crate::proxy::server::AppState;
use crate::scheduler::{credential_identity, AccountFingerprint, AccountId};
use crate::tui::ActivityEvent;

pub use crate::auth::codex_usage::ResetOutcome;

// ---------------------------------------------------------------------------
// Wire types (shared by the HTTP handlers, the CLI and the TUI)
// ---------------------------------------------------------------------------

/// Daemon-owned control metadata for one account, serialized additively onto
/// the dashboard/status account object. Every counter is optional: absent means
/// UNKNOWN (never "none left"), so a client can render "—" instead of "0".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageControlDoc {
    /// `rate_limit_reset_credits.available_count` — resets the account OWNS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_resets: Option<u64>,
    /// `applicable_available_count` — server-reported "usable right now".
    /// Undocumented semantics: display information, never a permission gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applicable_resets: Option<u64>,
    /// Entitlement rows, populated only by an explicit list read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credits: Vec<ResetCredit>,
    /// Epoch ms of the last SUCCESSFUL control refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh_ms: Option<u64>,
    /// Sanitized error of the last FAILED control operation. Retained next to
    /// (not instead of) the previous observations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_ms: Option<u64>,
    /// Epoch ms of the last terminal redemption outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reset_ms: Option<u64>,
    /// Idempotency key of a redemption whose outcome is UNCERTAIN. While set,
    /// only this id may be retried for this account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_credit_id: Option<String>,
}

/// One account's refresh outcome. A multi-account refresh reports every entry;
/// a partial failure is never silently green.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshResult {
    pub account: String,
    pub ok: bool,
    /// `"codex"` / `"oauth"` — the usage source actually used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Sanitized failure text; never a credential, URL or raw body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The account's control metadata after this attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_control: Option<UsageControlDoc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshResponse {
    /// `false` when ANY requested account failed.
    pub ok: bool,
    pub results: Vec<RefreshResult>,
}

/// `GET /llmux/reset-credits?account=` — a fresh entitlement read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetCreditsResponse {
    pub account: String,
    /// Resets owned (unknown when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_count: Option<u64>,
    /// Server-reported applicability, from the last usage read. Informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applicable_available_count: Option<u64>,
    pub credits: Vec<ResetCredit>,
    /// `true` when a NEW redemption may be attempted (a redeemable credit
    /// exists). Attempting is still the operator's decision; upstream decides.
    pub redeemable: bool,
    /// Set when the account owns resets that upstream currently reports as
    /// non-applicable — a warning to show, not a refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applicability_warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_request_id: Option<String>,
}

/// `POST /llmux/reset-credits/consume` body. The client OWNS
/// `redeem_request_id`; the daemon never substitutes or mints one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsumeRequest {
    pub account: String,
    pub redeem_request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_id: Option<String>,
    /// Explicit operator confirmation. Absent/false refuses before the network.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsumeResponse {
    pub outcome: ResetOutcome,
    /// Echoed verbatim — the key to reuse on an explicit retry.
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_id: Option<String>,
    pub windows_reset: i64,
    /// The follow-up usage/inventory read failed: the redemption still
    /// SUCCEEDED, the local view is stale. Not a retryable redemption failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_warning: Option<String>,
    /// `applicable_available_count == 0` at attempt time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applicability_warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_control: Option<UsageControlDoc>,
}

/// `POST /llmux/switch` response (additive: `refresh_warning`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchResponse {
    pub ok: bool,
    pub current: String,
    /// The pre-switch refresh failed; the switch itself did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_warning: Option<String>,
}

/// Minimal non-secret crash receipt written BEFORE a redemption POST.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingReset {
    pub account: String,
    /// Stable credential identity (see [`credential_identity`]) of the account
    /// that STARTED this redemption. Receipts are keyed by `(account,
    /// identity)`, so a replacement behind the same name gets its own row and
    /// can never inherit — or erase — its predecessor's.
    pub identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_id: Option<String>,
    pub request_id: String,
    pub started_at_ms: u64,
    /// Set when the credential this receipt belongs to is gone (replaced or
    /// removed). An invalidated row is RETAINED as evidence — the redemption
    /// really may have happened — but it never authorizes a retry, is never
    /// advertised as an account's pending redemption, and its request id is
    /// refused rather than re-sent under whatever credential now holds the
    /// name. Absent = live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidated_at_ms: Option<u64>,
}

impl PendingReset {
    /// Does this receipt BIND the account that currently carries `identity`?
    pub fn binds(&self, identity: &str) -> bool {
        self.invalidated_at_ms.is_none() && self.identity == identity
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, thiserror::Error)]
pub enum UsageControlError {
    #[error("unknown account {0}")]
    UnknownAccount(String),
    #[error("account {account} ({kind}) has no usage controls")]
    Unsupported { account: String, kind: String },
    #[error("{0}")]
    Invalid(String),
    #[error("another control operation is already running for account {0}")]
    Busy(String),
    /// The account was replaced or removed WHILE this operation was in flight.
    /// Its result belongs to a credential that no longer holds the name, so it
    /// is neither published nor acted on.
    #[error("account {0} was replaced or removed during the operation; nothing was applied")]
    AccountChanged(String),
    /// A redemption for this account is pending with a DIFFERENT id. ONLY the
    /// original id may retry — there is no abandon: the daemon releases a
    /// pending redemption exclusively on a terminal four-code outcome, so a
    /// client closing its dialog does not cancel anything. The pending id is
    /// exposed so an operator (or a reopened client) can resume.
    #[error("account {account} has a pending redemption (request id {request_id}); only that id may be retried — closing the client dialog does not cancel it")]
    Pending {
        account: String,
        request_id: String,
        credit_id: Option<String>,
    },
    #[error("{0}")]
    NoCredit(String),
    #[error("switch refused: {0}")]
    SwitchRefused(String),
    #[error("{0}")]
    Upstream(String),
    /// The redemption may or may not have spent a credit. The SAME id must be
    /// reused for any retry.
    #[error("redemption outcome is uncertain ({message}); retry with request id {request_id}")]
    Uncertain { message: String, request_id: String },
    #[error("could not record the pending redemption ({0}); no redemption was attempted")]
    Persistence(String),
}

impl UsageControlError {
    pub fn status(&self) -> http::StatusCode {
        use http::StatusCode as S;
        match self {
            Self::UnknownAccount(_) => S::NOT_FOUND,
            Self::Unsupported { .. } => S::UNPROCESSABLE_ENTITY,
            Self::Invalid(_) => S::BAD_REQUEST,
            Self::Busy(_)
            | Self::AccountChanged(_)
            | Self::Pending { .. }
            | Self::NoCredit(_)
            | Self::SwitchRefused(_) => S::CONFLICT,
            Self::Upstream(_) | Self::Uncertain { .. } => S::BAD_GATEWAY,
            Self::Persistence(_) => S::INTERNAL_SERVER_ERROR,
        }
    }

    /// Stable machine code, so clients branch on the kind rather than on text.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownAccount(_) => "unknown_account",
            Self::Unsupported { .. } => "unsupported",
            Self::Invalid(_) => "invalid_request",
            Self::Busy(_) => "busy",
            Self::AccountChanged(_) => "account_changed",
            Self::Pending { .. } => "pending_redemption",
            Self::NoCredit(_) => "no_credit_available",
            Self::SwitchRefused(_) => "switch_refused",
            Self::Upstream(_) => "upstream_error",
            Self::Uncertain { .. } => "uncertain",
            Self::Persistence(_) => "persistence_error",
        }
    }

    /// JSON body in the same envelope as every other llmux error, plus the
    /// machine-readable ids a client needs to continue safely.
    pub fn body(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "type": "error",
            "error": { "type": self.code(), "message": self.to_string() },
        });
        let map = value.as_object_mut().expect("json object");
        match self {
            Self::Pending {
                request_id,
                credit_id,
                ..
            } => {
                map.insert("pending_request_id".into(), request_id.as_str().into());
                if let Some(credit_id) = credit_id {
                    map.insert("pending_credit_id".into(), credit_id.as_str().into());
                }
            }
            Self::Uncertain { request_id, .. } => {
                map.insert("request_id".into(), request_id.as_str().into());
            }
            _ => {}
        }
        value
    }
}

// ---------------------------------------------------------------------------
// Store: per-account metadata, pending receipts, and the per-account try-lock
// ---------------------------------------------------------------------------

/// Daemon-owned usage-control state. Std locks are held for map reads/writes
/// only — never across IO or an `.await`.
#[derive(Debug, Default)]
pub struct UsageControlStore {
    meta: Mutex<HashMap<String, UsageControlDoc>>,
    pending: Mutex<PendingRegistry>,
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// GLOBAL serialization of the pending-receipt registry. The receipt file
    /// holds EVERY account, so the per-account lock is not enough: two accounts
    /// redeeming at once would each read-modify-write the same file and one
    /// crash receipt would be lost. Held across the (short, synchronous)
    /// load→mutate→save→mirror sequence and never across HTTP.
    registry: AsyncMutex<()>,
}

#[derive(Debug, Default)]
struct PendingRegistry {
    /// Receipts reloaded from disk once per process (crash recovery).
    loaded: bool,
    /// Keyed by `(account, identity)`: one LIVE row per credential plus the
    /// retained invalidated rows of its predecessors.
    entries: Vec<PendingReset>,
}

/// How many INVALIDATED receipts to retain per account. Evidence of a
/// redemption that may have happened is worth keeping, but not forever — the
/// newest few are what an operator can still act on.
const INVALIDATED_KEEP: usize = 4;

impl UsageControlStore {
    pub fn doc(&self, account: &str) -> Option<UsageControlDoc> {
        self.meta
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(account)
            .cloned()
    }

    /// Snapshot of every account's metadata, for the dashboard document.
    pub fn all(&self) -> HashMap<String, UsageControlDoc> {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn update<F: FnOnce(&mut UsageControlDoc)>(&self, account: &str, edit: F) -> UsageControlDoc {
        let mut meta = self.meta.lock().unwrap_or_else(|e| e.into_inner());
        let doc = meta.entry(account.to_string()).or_default();
        edit(doc);
        doc.clone()
    }

    /// Drop an account's observation METADATA: its credential was replaced or
    /// removed, so every reading attached to the old one is invalid.
    ///
    /// Deliberately does NOT touch the pending registry: a redemption receipt
    /// is not an observation, it is evidence that an irreversible call may have
    /// happened. Invalidating one is a persisted registry transaction
    /// ([`AppState::invalidate_pending`]); dropping it from memory alone would
    /// let the disk row resurrect on the next reload.
    pub fn forget(&self, account: &str) {
        self.meta
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(account);
    }

    /// Per-account exclusion: a competing control operation gets `None` (→
    /// 409 busy) rather than queueing behind this one. Queuing would let two
    /// different idempotency keys reach the redemption endpoint.
    fn try_lock(&self, account: &str) -> Option<OwnedMutexGuard<()>> {
        let lock = self
            .locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(account.to_string())
            .or_default()
            .clone();
        lock.try_lock_owned().ok()
    }

    /// Every receipt filed under this account name — live AND retained
    /// invalidated evidence.
    fn pending_rows(&self, account: &str) -> Vec<PendingReset> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .iter()
            .filter(|p| p.account == account)
            .cloned()
            .collect()
    }

    fn pending_all(&self) -> Vec<PendingReset> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .clone()
    }

    /// Merge crash receipts read from disk (once per process). Memory wins:
    /// this process's own in-flight receipts are never overwritten by a stale
    /// file read. Callers hold [`Self::registry`].
    fn adopt_disk(&self, disk: Vec<PendingReset>) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending.loaded {
            return;
        }
        for entry in disk {
            if !pending
                .entries
                .iter()
                .any(|p| p.account == entry.account && p.identity == entry.identity)
            {
                pending.entries.push(entry);
            }
        }
        pending.loaded = true;
    }

    /// Reload the crash receipts once per process, under the global registry
    /// lock so it cannot interleave with a [`Self::mutate_pending`].
    async fn ensure_loaded(&self, path: Option<&Path>) -> Result<(), UsageControlError> {
        let _guard = self.registry.lock().await;
        if self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .loaded
        {
            return Ok(());
        }
        self.adopt_disk(load_receipts(path)?);
        Ok(())
    }

    /// ONE atomic registry transaction: re-read the file (so receipts written
    /// by another process or for another account survive), apply `edit` to the
    /// merged row set, persist, then mirror into memory. The global lock makes
    /// concurrent accounts serialize instead of clobbering each other; a failed
    /// save leaves BOTH disk and memory untouched.
    async fn commit_pending<F>(&self, path: Option<&Path>, edit: F) -> Result<(), UsageControlError>
    where
        F: FnOnce(&mut Vec<PendingReset>),
    {
        let _guard = self.registry.lock().await;
        // Disk is the base, then this process's own rows (a receipt written
        // earlier here must not be dropped if the file changed underneath us).
        let mut rows = load_receipts(path)?;
        for mine in self.pending_all() {
            match rows
                .iter_mut()
                .find(|p| p.account == mine.account && p.identity == mine.identity)
            {
                Some(existing) => *existing = mine,
                None => rows.push(mine),
            }
        }
        edit(&mut rows);
        prune_invalidated(&mut rows);
        rows.sort_by(|a, b| (&a.account, &a.identity).cmp(&(&b.account, &b.identity)));
        save_receipts(path, &rows)?;
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries = rows;
        Ok(())
    }

    /// File a new receipt for `(account, identity)`. Any OTHER row of that
    /// account belongs to a credential that no longer holds the name: it is
    /// marked invalidated and KEPT as evidence, never dropped to make room.
    async fn open_pending(
        &self,
        path: Option<&Path>,
        receipt: PendingReset,
    ) -> Result<(), UsageControlError> {
        self.commit_pending(path, move |rows| {
            for row in rows.iter_mut() {
                if row.account == receipt.account
                    && row.identity != receipt.identity
                    && row.invalidated_at_ms.is_none()
                {
                    row.invalidated_at_ms = Some(now_ms());
                }
            }
            match rows
                .iter_mut()
                .find(|p| p.account == receipt.account && p.identity == receipt.identity)
            {
                Some(existing) => *existing = receipt,
                None => rows.push(receipt),
            }
        })
        .await
    }

    /// Release the receipt of `(account, identity)` — a TERMINAL four-code
    /// outcome, the only thing that ever clears a live receipt. Other rows of
    /// the same account (a predecessor's evidence) are untouched.
    async fn close_pending(
        &self,
        path: Option<&Path>,
        account: &str,
        identity: &str,
    ) -> Result<(), UsageControlError> {
        self.commit_pending(path, |rows| {
            rows.retain(|p| !(p.account == account && p.identity == identity));
        })
        .await
    }

    /// Mark every receipt of `account` not owned by `live` as invalidated IN
    /// MEMORY — synchronous and IO-free, so a roster change bites at the
    /// instant it happens. Returns `true` when something changed; the caller
    /// then persists the same decision. The durable copy is also caught up by
    /// any later [`Self::commit_pending`], which merges memory over disk.
    pub fn invalidate_in_memory(&self, account: &str, live: Option<&str>) -> bool {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = false;
        for row in pending.entries.iter_mut() {
            if row.account == account
                && row.invalidated_at_ms.is_none()
                && live != Some(row.identity.as_str())
            {
                row.invalidated_at_ms = Some(now_ms());
                changed = true;
            }
        }
        changed
    }

    /// Mark every receipt of `account` whose identity is NOT `live` as
    /// invalidated, persisted. Called when the credential behind a name is
    /// replaced or removed, so the row cannot resurrect from disk on the next
    /// reload and be mistaken for the successor's pending redemption.
    async fn invalidate_pending(
        &self,
        path: Option<&Path>,
        account: &str,
        live: Option<&str>,
    ) -> Result<(), UsageControlError> {
        let live = live.map(str::to_string);
        self.commit_pending(path, |rows| {
            for row in rows.iter_mut() {
                if row.account == account
                    && row.invalidated_at_ms.is_none()
                    && live.as_deref() != Some(row.identity.as_str())
                {
                    row.invalidated_at_ms = Some(now_ms());
                }
            }
        })
        .await
    }
}

/// Keep every LIVE receipt and only the newest [`INVALIDATED_KEEP`] invalidated
/// rows per account: evidence of a maybe-spent redemption is worth retaining,
/// but not without bound.
fn prune_invalidated(rows: &mut Vec<PendingReset>) {
    let mut per_account: HashMap<String, usize> = HashMap::new();
    rows.sort_by_key(|row| std::cmp::Reverse(row.started_at_ms));
    rows.retain(|row| {
        if row.invalidated_at_ms.is_none() {
            return true;
        }
        let seen = per_account.entry(row.account.clone()).or_insert(0);
        *seen += 1;
        *seen <= INVALIDATED_KEEP
    });
}

/// Read the persisted pending receipts. A missing file is "none pending"; an
/// unreadable/corrupt file is an ERROR — losing a receipt would let a fresh
/// key be spent after a crash.
fn load_receipts(path: Option<&Path>) -> Result<Vec<PendingReset>, UsageControlError> {
    let Some(path) = path else {
        return Err(UsageControlError::Persistence(
            "no daemon state directory".into(),
        ));
    };
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| UsageControlError::Persistence("pending receipt file is corrupt".into())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(UsageControlError::Persistence(err.kind().to_string())),
    }
}

/// Persist the receipts atomically (temp file + rename in the same directory),
/// so a crash mid-write cannot truncate the previous state.
fn save_receipts(path: Option<&Path>, entries: &[PendingReset]) -> Result<(), UsageControlError> {
    let Some(path) = path else {
        return Err(UsageControlError::Persistence(
            "no daemon state directory".into(),
        ));
    };
    let io = |err: std::io::Error| UsageControlError::Persistence(err.kind().to_string());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    let body = serde_json::to_vec(entries)
        .map_err(|_| UsageControlError::Persistence("receipt serialize failed".into()))?;
    // A UNIQUE temp name per write: a shared `<file>.tmp` makes two concurrent
    // writers race their own renames (the loser's rename hits ENOENT), which
    // would report "could not record the pending redemption" for a redemption
    // that was in fact about to be recorded.
    let tmp = path.with_extension(format!("tmp-{}", ulid::Ulid::new()));
    std::fs::write(&tmp, &body).map_err(io)?;
    let renamed = std::fs::rename(&tmp, path).map_err(io);
    if renamed.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    renamed
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// The usage source for a credential, or `None` when the provider has no
/// usage control at all (api key / openrouter / grok).
fn provider_of(credential: &AccountCredential) -> Option<&'static str> {
    match credential {
        AccountCredential::Codex { .. } => Some("codex"),
        AccountCredential::Oauth { .. } => Some("oauth"),
        _ => None,
    }
}

/// Everything captured BEFORE any IO. `fingerprint` (generation + credential
/// digest) is what an observation must still match to be APPLIED; `identity`
/// is the stable, restart-surviving key a PERSISTED pending redemption is
/// filed under.
struct Target {
    id: AccountId,
    credential: AccountCredential,
    identity: String,
    fingerprint: AccountFingerprint,
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

impl AppState {
    /// This account's daemon-owned control metadata (TUI/dashboard read).
    pub fn usage_control_doc(&self, account: &str) -> Option<UsageControlDoc> {
        self.usage_controls.doc(account)
    }

    /// The redemption that BINDS this account right now — the id an operator
    /// must reuse to retry. A receipt left behind by a credential that has
    /// since been replaced is NOT this account's pending redemption and is
    /// never returned here.
    pub fn pending_reset(&self, account: &str) -> Option<PendingReset> {
        let identity = self.target(account).ok()?.identity;
        self.usage_controls
            .pending_rows(account)
            .into_iter()
            .find(|p| p.binds(&identity))
    }

    /// Every receipt this daemon knows about, including reloaded ones and the
    /// retained invalidated evidence of replaced credentials (each flagged by
    /// `invalidated_at_ms`). Operator surface: raw, unfiltered.
    pub async fn pending_resets(&self) -> Vec<PendingReset> {
        let _ = self.hydrate_pending().await;
        self.usage_controls.pending_all()
    }

    /// Reload persisted receipts (once per process) and re-derive what each
    /// account's metadata should ADVERTISE, against the live credentials. This
    /// is what makes the display truthful after a restart — the doc is
    /// in-memory, the receipts are not — and what stops a replaced credential
    /// from inheriting its predecessor's pending id.
    pub(crate) async fn hydrate_pending(&self) -> Result<(), UsageControlError> {
        self.usage_controls
            .ensure_loaded(self.usage_control_state_path.as_deref())
            .await?;
        let mut accounts: Vec<String> = self
            .usage_controls
            .pending_all()
            .into_iter()
            .map(|p| p.account)
            .collect();
        accounts.sort();
        accounts.dedup();
        for account in accounts {
            let binding = self.pending_reset(&account);
            self.usage_controls.update(&account, |doc| {
                doc.pending_request_id = binding.as_ref().map(|p| p.request_id.clone());
                doc.pending_credit_id = binding.as_ref().and_then(|p| p.credit_id.clone());
            });
        }
        Ok(())
    }

    /// Persist the invalidation of every receipt this account's CURRENT
    /// credential does not own (it was replaced or removed). Evidence is
    /// retained and flagged, not deleted. Returns a sanitized warning when the
    /// invalidation could not be recorded — the in-memory and read-time
    /// identity checks still refuse the row, but the operator is told the
    /// durable record is stale rather than being shown a clean result.
    pub async fn invalidate_pending(&self, account: &str) -> Option<String> {
        let live = self.target(account).ok().map(|t| t.identity);
        // Memory FIRST (synchronous): the refusal must not wait on the disk.
        self.usage_controls.forget(account);
        self.usage_controls
            .invalidate_in_memory(account, live.as_deref());
        match self
            .usage_controls
            .invalidate_pending(
                self.usage_control_state_path.as_deref(),
                account,
                live.as_deref(),
            )
            .await
        {
            Ok(()) => None,
            Err(err) => Some(format!(
                "the pending-redemption record for {account} could not be updated ({err}); it is refused in memory but the stored copy is stale"
            )),
        }
    }

    /// A ROSTER change is itself an invalidation event: any name whose
    /// credential identity changed — or that vanished — loses its cached
    /// observations, counters and credit list immediately, and its pending
    /// receipts are flagged. `before` is the `(name, identity)` snapshot taken
    /// BEFORE the swap.
    ///
    /// The in-memory half runs synchronously so the very next dashboard frame
    /// is already truthful; the durable half is spawned (it is file IO and this
    /// is called from sync roster code). Losing the spawn is safe, not silent:
    /// every read filters by the live identity anyway, and the next registry
    /// commit re-persists the in-memory flags.
    pub fn invalidate_roster_changes(&self, before: &[(String, String)], merged: &Config) {
        let stale: Vec<String> = before
            .iter()
            .filter(|(name, identity)| {
                merged
                    .accounts
                    .iter()
                    .find(|a| &a.name == name)
                    .map(|a| credential_identity(&a.credential))
                    .as_ref()
                    != Some(identity)
            })
            .map(|(name, _)| name.clone())
            .collect();
        if stale.is_empty() {
            return;
        }
        for name in &stale {
            self.usage_controls.forget(name);
            self.usage_controls.invalidate_in_memory(name, None);
            tracing::info!(account = %name, "usage-control state invalidated (roster change)");
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let state = self.clone();
            handle.spawn(async move {
                for name in stale {
                    if let Some(warning) = state.invalidate_pending(&name).await {
                        tracing::warn!(account = %name, warning, "pending receipt not persisted");
                    }
                }
            });
        }
    }

    /// Re-assert, AFTER an await, that the account still carries the exact
    /// credential the operation started with. Every publication of an IO result
    /// — the inventory list, the receipt, the redemption POST itself — goes
    /// through this: reading is not the only thing that must not land on a
    /// successor, SPENDING must not either.
    fn still_current(&self, target: &Target) -> Result<(), UsageControlError> {
        if self.pool.fingerprint(&target.id).as_ref() == Some(&target.fingerprint) {
            return Ok(());
        }
        Err(UsageControlError::AccountChanged(target.id.0.clone()))
    }

    /// Resolve a name to a live account, its credential and its fingerprint,
    /// BEFORE any IO. The fingerprint is read from the same pool state as the
    /// credential, so the pair is self-consistent.
    fn target(&self, account: &str) -> Result<Target, UsageControlError> {
        let id = AccountId(account.to_string());
        let unknown = || UsageControlError::UnknownAccount(account.to_string());
        let credential = self.pool.credential(&id).ok_or_else(unknown)?;
        let fingerprint = self.pool.fingerprint(&id).ok_or_else(unknown)?;
        if fingerprint.digest != crate::scheduler::credential_digest(&credential) {
            // The roster moved between the two reads — retry rather than act
            // on a mismatched pair.
            return Err(UsageControlError::Busy(account.to_string()));
        }
        Ok(Target {
            id,
            identity: credential_identity(&credential),
            credential,
            fingerprint,
        })
    }

    fn supported_target(&self, account: &str) -> Result<(Target, &'static str), UsageControlError> {
        let target = self.target(account)?;
        let provider =
            provider_of(&target.credential).ok_or_else(|| UsageControlError::Unsupported {
                account: account.to_string(),
                kind: target.credential.kind().to_string(),
            })?;
        Ok((target, provider))
    }

    /// The WHAM base derived from the configured codex upstream. Refuses
    /// (422) rather than falling back to production.
    fn wham_base(&self) -> Result<String, UsageControlError> {
        codex_usage::wham_base(&self.config.codex.upstream).map_err(|err| {
            UsageControlError::Unsupported {
                account: "codex".into(),
                kind: err.sanitized(),
            }
        })
    }

    /// Explicit usage refresh: one account, or every account whose provider
    /// supports it. Bypasses scheduler eligibility and the idle-probe cooldown;
    /// changes no pause/health/operator state and sends NO inference request.
    pub async fn refresh_usage(
        &self,
        account: Option<&str>,
    ) -> Result<RefreshResponse, UsageControlError> {
        let targets: Vec<String> = match account {
            Some(name) => {
                // Hard errors (unknown / unsupported) are refused up front, so
                // a single-account call answers 404/422 rather than a green
                // envelope carrying a failure.
                self.supported_target(name)?;
                vec![name.to_string()]
            }
            None => self
                .pool
                .snapshot()
                .accounts
                .iter()
                .filter(|a| matches!(a.credential_kind, "codex" | "oauth"))
                .map(|a| a.id.0.clone())
                .collect(),
        };
        let mut results = Vec::with_capacity(targets.len());
        for name in targets {
            // Sequential by design: a burst of one call per account can trip
            // the upstream's own request-rate limit (same rationale as the
            // usage poller's global gap).
            results.push(match self.refresh_one(&name).await {
                Ok(result) => result,
                Err(err) if account.is_some() => return Err(err),
                Err(err) => RefreshResult {
                    account: name,
                    ok: false,
                    provider: None,
                    error: Some(err.to_string()),
                    usage_control: None,
                },
            });
        }
        Ok(RefreshResponse {
            ok: results.iter().all(|r| r.ok),
            results,
        })
    }

    async fn refresh_one(&self, account: &str) -> Result<RefreshResult, UsageControlError> {
        let (target, provider) = self.supported_target(account)?;
        let _guard = self
            .usage_controls
            .try_lock(account)
            .ok_or_else(|| UsageControlError::Busy(account.to_string()))?;
        let outcome = self.read_usage(&target).await;
        Ok(self
            .commit_refresh(account, provider, &target, outcome)
            .await)
    }

    /// [`Self::apply_refresh`] plus the persisted consequence of a DISCARDED
    /// result: the credential that owned any pending redemption for this name
    /// is gone, so its receipt is invalidated durably (and the failure to do so
    /// is reported, not swallowed).
    async fn commit_refresh(
        &self,
        account: &str,
        provider: &'static str,
        target: &Target,
        outcome: Result<CodexUsage, UsageControlError>,
    ) -> RefreshResult {
        let (mut result, discarded) = self.apply_refresh(account, provider, target, outcome);
        if discarded {
            if let Some(warning) = self.invalidate_pending(account).await {
                result.error = Some(match result.error.take() {
                    Some(error) => format!("{error}; {warning}"),
                    None => warning,
                });
            }
        }
        result
    }

    /// The provider-specific GET. Codex reads WHAM (which also carries the
    /// reset counters); Anthropic oauth reuses the existing usage helper.
    async fn read_usage(&self, target: &Target) -> Result<CodexUsage, UsageControlError> {
        match &target.credential {
            AccountCredential::Codex {
                access_token,
                account_id,
                ..
            } => {
                let base = self.wham_base()?;
                codex_usage::fetch_usage(
                    &self.client,
                    &base,
                    access_token,
                    account_id,
                    SystemTime::now(),
                )
                .await
                .map_err(upstream)
            }
            AccountCredential::Oauth { access_token, .. } => {
                let fetch = crate::scheduler::usage::fetch_usage(
                    &self.client,
                    &self.config.upstream,
                    access_token,
                );
                match tokio::time::timeout(codex_usage::CONTROL_TIMEOUT, fetch).await {
                    Ok(Ok(usage)) => Ok(CodexUsage {
                        usage,
                        ..Default::default()
                    }),
                    // The anthropic helper's error Display is already
                    // credential-free, but keep the sanitized phrasing.
                    Ok(Err(_)) => Err(UsageControlError::Upstream(
                        "usage endpoint request failed".into(),
                    )),
                    Err(_) => Err(UsageControlError::Upstream(
                        "usage endpoint request timed out".into(),
                    )),
                }
            }
            other => Err(UsageControlError::Unsupported {
                account: target.id.0.clone(),
                kind: other.kind().to_string(),
            }),
        }
    }

    /// Commit (or record the failure of) one refresh. A failure retains every
    /// previous observation and counter; only success moves them.
    ///
    /// Returns `(result, discarded)`: `discarded` means the read SUCCEEDED but
    /// the account it belongs to was replaced/removed in flight, so the result
    /// was thrown away. It is an INTERNAL signal (the wire type is stable for
    /// the CLI/TUI) that tells [`Self::commit_refresh`] to invalidate the
    /// predecessor's durable state.
    fn apply_refresh(
        &self,
        account: &str,
        provider: &'static str,
        target: &Target,
        outcome: Result<CodexUsage, UsageControlError>,
    ) -> (RefreshResult, bool) {
        let failure = |state: &Self, error: String| RefreshResult {
            account: account.to_string(),
            ok: false,
            provider: Some(provider.to_string()),
            usage_control: Some(state.usage_controls.update(account, |doc| {
                doc.last_error = Some(error.clone());
                doc.last_error_ms = Some(now_ms());
            })),
            error: Some(error),
        };
        let fresh = match outcome {
            Ok(fresh) => fresh,
            Err(err) => return (failure(self, err.to_string()), false),
        };
        // Generation + credential-digest revalidation happens INSIDE the pool
        // write lock: an account removed, re-added or re-credentialed during
        // the request must not receive its predecessor's reading.
        if !self.pool.record_usage_if(
            &target.id,
            &target.fingerprint,
            &fresh.usage,
            SystemTime::now(),
        ) {
            return (
                RefreshResult {
                    account: account.to_string(),
                    ok: false,
                    provider: Some(provider.to_string()),
                    error: Some(
                        "account credential changed during the refresh; the result was discarded"
                            .into(),
                    ),
                    usage_control: None,
                },
                true,
            );
        }
        let doc = self.usage_controls.update(account, |doc| {
            // Absent counters stay UNKNOWN rather than overwriting a known
            // value with `None` — the usage body omits them for non-codex.
            if fresh.available_resets.is_some() {
                doc.available_resets = fresh.available_resets;
            }
            if fresh.applicable_resets.is_some() {
                doc.applicable_resets = fresh.applicable_resets;
            }
            doc.last_refresh_ms = Some(now_ms());
            doc.last_error = None;
            doc.last_error_ms = None;
        });
        (
            RefreshResult {
                account: account.to_string(),
                ok: true,
                provider: Some(provider.to_string()),
                error: None,
                usage_control: Some(doc),
            },
            false,
        )
    }

    /// Fresh entitlement list for one codex account. A pure read: it never
    /// redeems anything.
    pub async fn reset_credits(
        &self,
        account: &str,
    ) -> Result<ResetCreditsResponse, UsageControlError> {
        let target = self.codex_target(account)?;
        let _guard = self
            .usage_controls
            .try_lock(account)
            .ok_or_else(|| UsageControlError::Busy(account.to_string()))?;
        // Truthful pending fields, including after a restart (receipts live on
        // disk, the metadata does not) and after a replacement (a predecessor's
        // receipt is not this credential's pending redemption).
        self.hydrate_pending().await?;
        let credits = self.read_credits(&target).await?;
        // The list is IO: publish it only if it still describes the account
        // that currently holds this name.
        self.still_current(&target)?;
        Ok(self.credits_response(account, credits))
    }

    fn codex_target(&self, account: &str) -> Result<Target, UsageControlError> {
        let target = self.target(account)?;
        if !matches!(target.credential, AccountCredential::Codex { .. }) {
            return Err(UsageControlError::Unsupported {
                account: account.to_string(),
                kind: target.credential.kind().to_string(),
            });
        }
        Ok(target)
    }

    async fn read_credits(&self, target: &Target) -> Result<ResetCredits, UsageControlError> {
        let AccountCredential::Codex {
            access_token,
            account_id,
            ..
        } = &target.credential
        else {
            return Err(UsageControlError::Unsupported {
                account: target.id.0.clone(),
                kind: target.credential.kind().to_string(),
            });
        };
        let base = self.wham_base()?;
        codex_usage::fetch_reset_credits(&self.client, &base, access_token, account_id)
            .await
            .map_err(upstream)
    }

    fn credits_response(&self, account: &str, credits: ResetCredits) -> ResetCreditsResponse {
        let redeemable = redeemable_credit(&credits, None).is_some();
        let doc = self.usage_controls.update(account, |doc| {
            doc.credits = credits.credits.clone();
            if credits.available_count.is_some() {
                doc.available_resets = credits.available_count;
            }
            doc.last_refresh_ms = Some(now_ms());
            doc.last_error = None;
            doc.last_error_ms = None;
        });
        ResetCreditsResponse {
            account: account.to_string(),
            available_count: credits.available_count.or(doc.available_resets),
            applicable_available_count: doc.applicable_resets,
            credits: credits.credits,
            redeemable,
            applicability_warning: applicability_warning(
                doc.available_resets,
                doc.applicable_resets,
            ),
            // From the receipt registry (filtered to the credential that holds
            // the name RIGHT NOW), never from whatever the metadata happens to
            // remember.
            pending_request_id: self.pending_reset(account).map(|p| p.request_id),
        }
    }

    /// Redeem exactly ONE reset credit — the only irreversible operation in
    /// this module.
    ///
    /// Order is load-bearing: validate → per-account try-lock → pending-receipt
    /// check → (new redemption only) fresh inventory gate → persist receipt →
    /// POST → terminal outcome clears the receipt / uncertain keeps it.
    pub async fn consume_reset(
        &self,
        request: &ConsumeRequest,
    ) -> Result<ConsumeResponse, UsageControlError> {
        let request_id = request.redeem_request_id.trim();
        if !request.confirm {
            return Err(UsageControlError::Invalid(
                "redeeming a reset requires explicit confirmation".into(),
            ));
        }
        if request_id.is_empty() {
            return Err(UsageControlError::Invalid(
                "redeem_request_id is required (the client owns the idempotency key)".into(),
            ));
        }
        let credit_id = match request.credit_id.as_deref().map(str::trim) {
            Some("") => {
                return Err(UsageControlError::Invalid(
                    "credit_id must be non-empty when supplied".into(),
                ))
            }
            other => other.map(str::to_string),
        };
        let target = self.codex_target(&request.account)?;
        let base = self.wham_base()?;
        let account = request.account.as_str();

        let _guard = self
            .usage_controls
            .try_lock(account)
            .ok_or_else(|| UsageControlError::Busy(account.to_string()))?;

        // Crash recovery: a receipt written by a previous process still binds
        // this account (a restart must not spend a fresh key).
        self.hydrate_pending().await?;

        let rows = self.usage_controls.pending_rows(account);
        // A receipt that does NOT bind the current credential — a predecessor's,
        // or one already invalidated — can neither authorize a retry nor have
        // its id re-sent under the credential that now holds the name. Refusing
        // by id (rather than silently starting a NEW redemption that happens to
        // reuse the string) is what keeps the old key off the wire.
        if let Some(stale) = rows
            .iter()
            .find(|p| p.request_id == request_id && !p.binds(&target.identity))
        {
            return Err(UsageControlError::Invalid(format!(
                "request id {} was issued for a previous credential of account {account} \
                 (identity {}) and cannot be redeemed under the current one; \
                 its outcome stays unresolved and a new redemption needs a new id",
                stale.request_id, stale.identity
            )));
        }
        let pending = rows.into_iter().find(|p| p.binds(&target.identity));
        let (credit_id, applicability_warning) = match &pending {
            Some(pending) if pending.request_id == request_id => {
                // Retry of an UNCERTAIN redemption: the first attempt may have
                // already consumed the credit, so the fresh-inventory gate is
                // deliberately skipped and the ORIGINAL credit is kept.
                if credit_id.is_some() && credit_id != pending.credit_id {
                    return Err(UsageControlError::Invalid(
                        "a retry must keep the original credit id".into(),
                    ));
                }
                (pending.credit_id.clone(), None)
            }
            Some(pending) => {
                return Err(UsageControlError::Pending {
                    account: account.to_string(),
                    request_id: pending.request_id.clone(),
                    credit_id: pending.credit_id.clone(),
                })
            }
            None => {
                // A NEW redemption needs a successful, fresh entitlement read
                // showing a redeemable credit. `applicable_available_count` is
                // shown, not enforced: its semantics are undocumented and the
                // consume response is the authority.
                let credits = self.read_credits(&target).await?;
                // The inventory read is IO too: a credential replaced while it
                // was in flight must not have its list published, must not
                // produce a receipt, and must not be spent.
                self.still_current(&target)?;
                let chosen =
                    redeemable_credit(&credits, credit_id.as_deref()).ok_or_else(|| {
                        UsageControlError::NoCredit(match credit_id.as_deref() {
                            Some(id) => format!("credit {id} is not available for redemption"),
                            None => format!("account {account} has no redeemable reset credit"),
                        })
                    })?;
                let response = self.credits_response(account, credits);
                (chosen, response.applicability_warning)
            }
        };

        // Durable BEFORE the irreversible call. A save failure aborts: an
        // unrecorded redemption could be re-attempted with a fresh key.
        if pending.is_none() {
            let receipt = PendingReset {
                account: account.to_string(),
                identity: target.identity.clone(),
                credit_id: credit_id.clone(),
                request_id: request_id.to_string(),
                started_at_ms: now_ms(),
                invalidated_at_ms: None,
            };
            // One atomic registry transaction (global lock + re-read + write),
            // so a concurrent redemption on a DIFFERENT account cannot drop
            // this receipt — and a predecessor's row is invalidated, not
            // overwritten.
            self.usage_controls
                .open_pending(self.usage_control_state_path.as_deref(), receipt.clone())
                .await?;
            self.usage_controls.update(account, |doc| {
                doc.pending_request_id = Some(receipt.request_id.clone());
                doc.pending_credit_id = receipt.credit_id.clone();
            });
        }

        let AccountCredential::Codex {
            access_token,
            account_id,
            ..
        } = &target.credential
        else {
            return Err(UsageControlError::Unsupported {
                account: account.to_string(),
                kind: target.credential.kind().to_string(),
            });
        };
        // LAST gate before the irreversible call, covering both the new-receipt
        // and the retry path: never spend a credit with a credential the pool
        // has already replaced.
        self.still_current(&target)?;
        let result = codex_usage::consume_reset_credit(
            &self.client,
            &base,
            access_token,
            account_id,
            request_id,
            credit_id.as_deref(),
        )
        .await;

        let result = match result {
            Ok(result) => result,
            Err(err) => {
                // Transport failure, malformed body or an unknown code: the
                // credit MAY be spent. Keep the receipt so only this id can
                // retry, and say so.
                self.usage_controls.update(account, |doc| {
                    doc.last_error = Some(err.sanitized());
                    doc.last_error_ms = Some(now_ms());
                });
                return Err(UsageControlError::Uncertain {
                    message: err.sanitized(),
                    request_id: request_id.to_string(),
                });
            }
        };

        // Terminal outcome: release the pending identity — DURABLY FIRST. If
        // the release cannot be written, the outcome stays terminal (upstream
        // already answered) but the HOLD IS RETAINED: the receipt still blocks
        // on disk, so telling the client the hold is gone would invite a second
        // redemption under a fresh key. Every warning below ACCUMULATES; a
        // later one never overwrites an earlier one.
        let mut warnings: Vec<String> = Vec::new();
        let released = self
            .usage_controls
            .close_pending(
                self.usage_control_state_path.as_deref(),
                account,
                &target.identity,
            )
            .await;
        if let Err(err) = &released {
            warnings.push(format!(
                "the redemption is complete but its pending record could not be released ({err}); \
                 request id {request_id} stays held — retry with THAT id, never a new one"
            ));
        }
        self.usage_controls.update(account, |doc| {
            if released.is_ok() {
                doc.pending_request_id = None;
                doc.pending_credit_id = None;
            }
            doc.last_error = None;
            doc.last_error_ms = None;
            doc.last_reset_ms = Some(now_ms());
        });

        // On reset / already_redeemed the local view is stale: re-read usage
        // AND inventory. A FAILED re-read of EITHER is a stale-read warning on
        // a SUCCESSFUL redemption — never a retryable redemption failure, and
        // never silence.
        if result.outcome.spent() {
            let usage = self.read_usage(&target).await;
            let applied = self.commit_refresh(account, "codex", &target, usage).await;
            if !applied.ok {
                warnings.push(format!(
                    "redemption succeeded but the follow-up usage read failed: {}",
                    applied.error.unwrap_or_else(|| "unknown error".into())
                ));
            }
            match self.read_credits(&target).await {
                Ok(credits) => {
                    self.credits_response(account, credits);
                }
                Err(err) => warnings.push(format!(
                    "redemption succeeded but the follow-up reset-credit read failed: {err}"
                )),
            }
        }
        Ok(ConsumeResponse {
            outcome: result.outcome,
            request_id: request_id.to_string(),
            credit_id,
            windows_reset: result.windows_reset,
            refresh_warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
            applicability_warning,
            usage_control: self.usage_controls.doc(account),
        })
    }

    /// Manual switch (TUI `s` / `POST /llmux/switch`): refresh the target's
    /// usage FIRST — including an already-active target — then commit through
    /// the pool, which stays the authority on pause / auth / cooldown.
    /// A failed refresh never blocks the switch; it rides back as a warning.
    pub async fn manual_switch(&self, account: &str) -> Result<SwitchResponse, UsageControlError> {
        // An unknown target is a switch REFUSAL (409), not a 404: that is the
        // pre-existing `/llmux/switch` contract and this path keeps it.
        let target = self
            .target(account)
            .map_err(|_| UsageControlError::SwitchRefused(format!("unknown account {account}")))?;
        let refresh_warning = match provider_of(&target.credential) {
            // An unsupported provider keeps the plain switch semantics.
            None => None,
            Some(provider) => match self.usage_controls.try_lock(account) {
                None => Some(format!(
                    "usage refresh skipped: {}",
                    UsageControlError::Busy(account.to_string())
                )),
                Some(_guard) => {
                    let outcome = self.read_usage(&target).await;
                    let result = self
                        .commit_refresh(account, provider, &target, outcome)
                        .await;
                    result
                        .error
                        .map(|err| format!("usage refresh failed: {err}"))
                }
            },
        };
        let now = SystemTime::now();
        let from = self
            .pool
            .snapshot()
            .representative_current()
            .map(|c| c.0.clone());
        self.pool
            .switch_to_checked(
                &target.id,
                Some(&target.fingerprint),
                &self.select_params(),
                now,
            )
            .map_err(|err| UsageControlError::SwitchRefused(err.to_string()))?;
        self.emit(ActivityEvent::AccountSwitched {
            from,
            to: target.id.0.clone(),
            reason: Some("manual".into()),
        });
        Ok(SwitchResponse {
            ok: true,
            current: target.id.0,
            refresh_warning,
        })
    }
}

/// Pick the credit to redeem: the explicitly requested one (which must be
/// redeemable), else the earliest-expiring redeemable row. `None` when the
/// account has nothing redeemable — the gate on a NEW redemption.
///
/// Returns the chosen credit's id. `None` means the row carried no id — a
/// defensively tolerated shape, not an observed one (the codex client's type
/// requires `id`) — in which case `credit_id` is omitted from the request and
/// upstream picks the next available credit itself.
fn redeemable_credit(credits: &ResetCredits, requested: Option<&str>) -> Option<Option<String>> {
    // The gate is a POSITIVE owned count. UNKNOWN is not permission: the count
    // is upstream's own answer to "is there anything to spend", and a list that
    // omits it has not answered. Zero is likewise a refusal.
    if !credits.available_count.is_some_and(|owned| owned > 0) {
        return None;
    }
    let rows: Vec<&ResetCredit> = credits
        .credits
        .iter()
        .filter(|c| c.is_redeemable())
        .collect();
    if let Some(requested) = requested {
        return rows
            .iter()
            .find(|c| c.id.as_deref() == Some(requested))
            .map(|c| c.id.clone());
    }
    if rows.is_empty() {
        return None;
    }
    // Earliest expiration first; rows without an expiry sort last.
    let chosen = rows
        .iter()
        .min_by_key(|c| c.expires_at.clone().unwrap_or_else(|| "~".into()))?;
    Some(chosen.id.clone())
}

/// The server says this account owns resets but reports none applicable right
/// now. Informational — the operator may still attempt a redemption.
fn applicability_warning(available: Option<u64>, applicable: Option<u64>) -> Option<String> {
    match (available, applicable) {
        (Some(owned), Some(0)) if owned > 0 => Some(format!(
            "owned {owned} · currently applicable 0 (server-reported); upstream may answer nothing_to_reset or no_credit"
        )),
        _ => None,
    }
}

fn upstream(err: CodexUsageError) -> UsageControlError {
    UsageControlError::Upstream(err.sanitized())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credit(id: &str, status: &str, expires: Option<&str>) -> ResetCredit {
        ResetCredit {
            id: Some(id.into()),
            reset_type: Some(codex_usage::CODEX_RATE_LIMITS.into()),
            status: Some(status.into()),
            expires_at: expires.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn zero_available_count_blocks_a_new_redemption() {
        let credits = ResetCredits {
            available_count: Some(0),
            credits: vec![credit("c1", "available", None)],
        };
        assert!(redeemable_credit(&credits, None).is_none());
    }

    #[test]
    fn unknown_available_count_blocks_a_new_redemption_even_with_a_listed_credit() {
        // An available-looking ROW is not permission when upstream did not say
        // how many the account owns.
        let credits = ResetCredits {
            available_count: None,
            credits: vec![credit("c1", "available", None)],
        };
        assert!(redeemable_credit(&credits, None).is_none());
        assert!(
            redeemable_credit(&credits, Some("c1")).is_none(),
            "naming the credit does not bypass the count gate"
        );
    }

    #[test]
    fn unknown_inventory_blocks_a_new_redemption() {
        // No rows and no count: unknown is not "go ahead".
        let credits = ResetCredits::default();
        assert!(redeemable_credit(&credits, None).is_none());
    }

    #[test]
    fn earliest_expiring_available_credit_is_chosen() {
        let credits = ResetCredits {
            available_count: Some(3),
            credits: vec![
                credit("late", "available", Some("2026-10-05T00:00:00Z")),
                credit("early", "available", Some("2026-09-21T00:00:00Z")),
                credit("redeemed", "redeemed", Some("2026-09-01T00:00:00Z")),
            ],
        };
        assert_eq!(
            redeemable_credit(&credits, None),
            Some(Some("early".into()))
        );
        assert_eq!(
            redeemable_credit(&credits, Some("late")),
            Some(Some("late".into()))
        );
        assert_eq!(
            redeemable_credit(&credits, Some("redeemed")),
            None,
            "an explicitly requested non-available credit is refused"
        );
    }

    #[test]
    fn applicability_zero_is_a_warning_only() {
        assert!(applicability_warning(Some(3), Some(0)).is_some());
        assert_eq!(
            applicability_warning(Some(3), None),
            None,
            "absent applicability is unknown, not a warning"
        );
        assert_eq!(applicability_warning(Some(0), Some(0)), None);
    }

    #[test]
    fn error_bodies_expose_the_ids_a_client_needs() {
        let pending = UsageControlError::Pending {
            account: "cx".into(),
            request_id: "rid-1".into(),
            credit_id: Some("c1".into()),
        };
        assert_eq!(pending.status(), http::StatusCode::CONFLICT);
        assert_eq!(pending.body()["pending_request_id"], "rid-1");
        let uncertain = UsageControlError::Uncertain {
            message: "upstream request timed out".into(),
            request_id: "rid-2".into(),
        };
        assert_eq!(uncertain.status(), http::StatusCode::BAD_GATEWAY);
        assert_eq!(uncertain.body()["request_id"], "rid-2");
        assert!(uncertain.to_string().contains("rid-2"));
    }
}
