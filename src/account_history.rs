//! Bounded, credential-free account lifecycle receipts. No provider bodies,
//! token values or credential hashes cross this boundary. A private snapshot
//! survives restart; a bounded writer queue keeps disk IO off request locks.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub const PER_ACCOUNT_LIMIT: usize = 64;
const TOTAL_LIMIT: usize = 2048;
const FILE_LIMIT: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountEvent {
    pub account: String,
    pub first_at_ms: u64,
    pub at_ms: u64,
    pub source: String,
    pub outcome: String,
    pub http_status: Option<u16>,
    pub reason: String,
    pub effect: String,
    pub retry_at_ms: Option<u64>,
    pub consecutive_failures: u32,
    pub count: u32,
}

impl AccountEvent {
    /// Reasons/effects are application-owned phrases, never provider body text.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        account: &str,
        source: &str,
        outcome: &str,
        status: Option<u16>,
        reason: &str,
        effect: &str,
        at: SystemTime,
    ) -> Self {
        let ms = epoch_ms(at);
        Self {
            account: safe_text(account, 256),
            first_at_ms: ms,
            at_ms: ms,
            source: safe_text(source, 48),
            outcome: safe_text(outcome, 32),
            http_status: status,
            reason: safe_text(reason, 240),
            effect: safe_text(effect, 96),
            retry_at_ms: None,
            consecutive_failures: 0,
            count: 1,
        }
    }
    pub fn with_retry(mut self, at: SystemTime, failures: u32) -> Self {
        self.retry_at_ms = Some(epoch_ms(at));
        self.consecutive_failures = failures;
        self
    }
    fn same_observation(&self, other: &Self) -> bool {
        self.account == other.account
            && self.source == other.source
            && self.outcome == other.outcome
            && self.http_status == other.http_status
            && self.reason == other.reason
            && self.effect == other.effect
    }
}

pub fn epoch_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn safe_text(text: &str, limit: usize) -> String {
    crate::proxy::logging::mask_credentials(text)
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect()
}

/// Interpret only known OAuth categories and descriptions. Unknown text is
/// deliberately omitted, even when it looks harmless: bodies can echo secrets.
pub fn oauth_reason(body: &str) -> &'static str {
    let value = serde_json::from_str::<serde_json::Value>(body).ok();
    let code = value
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(|v| v.as_str());
    let description = value
        .as_ref()
        .and_then(|v| v.get("error_description"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match code {
        Some("invalid_grant") if description == "refresh token expired" => {
            "invalid_grant: refresh token expired"
        }
        Some("invalid_grant") if description == "refresh token revoked" => {
            "invalid_grant: refresh token revoked"
        }
        Some("invalid_grant") => {
            "invalid_grant: provider rejected the refresh grant (cause undisclosed)"
        }
        Some("invalid_client") => "invalid_client: OAuth client rejected",
        Some("temporarily_unavailable") => "temporarily_unavailable: token service unavailable",
        Some("invalid_request") => "invalid_request: token request rejected",
        _ => "token endpoint rejected the request (unrecognized body omitted)",
    }
}

#[derive(Default)]
struct State {
    events: VecDeque<AccountEvent>,
    writer: Option<mpsc::SyncSender<()>>,
}

#[derive(Default)]
pub struct AccountHistory {
    state: Arc<Mutex<State>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Drop for AccountHistory {
    fn drop(&mut self) {
        // Closing the wake channel drains the latest pending snapshot before
        // the state disappears on graceful daemon shutdown/restart.
        self.lock().writer.take();
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = worker.join();
        }
    }
}

impl AccountHistory {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Called at daemon startup only. Missing old files mean unknown history.
    pub fn arm(&self, path: PathBuf) -> std::io::Result<()> {
        let history = match read_history(&path) {
            Ok(history) => history,
            Err(err) => {
                tracing::warn!(error = %err, "old account history unreadable; new receipts will still persist");
                VecDeque::new()
            }
        };
        // The channel carries only wakeups. The latest complete snapshot
        // remains in shared memory, so a full wake queue never loses it.
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        let shared = Arc::downgrade(&self.state);
        let worker = std::thread::Builder::new()
            .name("account-history".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    let Some(shared) = shared.upgrade() else {
                        break;
                    };
                    let snapshot = shared
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .events
                        .clone();
                    if let Err(err) = write_history(&path, &snapshot) {
                        tracing::warn!(error = %err, "account history persistence failed");
                    }
                }
            })?;
        let mut state = self.lock();
        state.events = history;
        state.writer = Some(tx);
        *self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
        Ok(())
    }
    pub fn record(&self, event: AccountEvent) {
        let mut state = self.lock();
        // Coalesce only the latest receipt for THIS account. An intervening
        // refresh/failure/relogin remains a separate chronological milestone.
        if let Some(index) = state
            .events
            .iter()
            .rposition(|e| e.account == event.account)
        {
            if state.events[index].same_observation(&event) {
                if let Some(mut previous) = state.events.remove(index) {
                    previous.at_ms = event.at_ms;
                    previous.retry_at_ms = event.retry_at_ms;
                    previous.consecutive_failures = event.consecutive_failures;
                    previous.count = previous.count.saturating_add(1);
                    state.events.push_back(previous);
                }
            } else {
                state.events.push_back(event);
            }
        } else {
            state.events.push_back(event);
        }
        trim(&mut state.events);
        if let Some(writer) = &state.writer {
            if let Err(mpsc::TrySendError::Disconnected(())) = writer.try_send(()) {
                tracing::warn!("account history writer unavailable");
            }
        }
    }
    /// Newest-first per account, shared by local and attached dashboards.
    pub fn snapshot(&self) -> HashMap<String, Vec<AccountEvent>> {
        let mut rows: HashMap<String, Vec<AccountEvent>> = HashMap::new();
        for event in self.lock().events.iter().rev() {
            rows.entry(event.account.clone())
                .or_default()
                .push(event.clone());
        }
        rows
    }
}

fn trim(events: &mut VecDeque<AccountEvent>) {
    let mut counts = HashMap::<String, usize>::new();
    let mut retained = VecDeque::new();
    for event in events.drain(..).rev().take(TOTAL_LIMIT) {
        let count = counts.entry(event.account.clone()).or_default();
        if *count < PER_ACCOUNT_LIMIT {
            *count += 1;
            retained.push_front(event);
        }
    }
    *events = retained;
}

fn read_history(path: &Path) -> std::io::Result<VecDeque<AccountEvent>> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(VecDeque::new()),
        Err(err) => return Err(err),
    };
    let mut bytes = Vec::new();
    file.take(FILE_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err(std::io::Error::other("account history exceeds size limit"));
    }
    let mut events: VecDeque<AccountEvent> =
        serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
    for event in &mut events {
        event.account = safe_text(&event.account, 256);
        event.source = safe_text(&event.source, 48);
        event.outcome = safe_text(&event.outcome, 32);
        event.reason = safe_text(&event.reason, 240);
        event.effect = safe_text(&event.effect, 96);
    }
    trim(&mut events);
    Ok(events)
}

fn write_history(path: &Path, events: &VecDeque<AccountEvent>) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // UTF-8 labels and JSON escaping can exceed the replay byte limit even
    // with the row count bounded. Persist the newest complete suffix that
    // fits, so a successful write always remains readable on restart.
    let mut rows = Vec::new();
    let mut bytes = 2_u64; // array brackets
    for event in events.iter().rev() {
        let row = serde_json::to_vec(event).map_err(std::io::Error::other)?;
        let growth = row.len() as u64 + u64::from(!rows.is_empty());
        if bytes + growth > FILE_LIMIT {
            break;
        }
        bytes += growth;
        rows.push(row);
    }
    let temporary = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(b"[")?;
    for (index, row) in rows.iter().rev().enumerate() {
        if index > 0 {
            file.write_all(b",")?;
        }
        file.write_all(row)?;
    }
    file.write_all(b"]")?;
    file.flush()?;
    std::fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TestDir(PathBuf);
    impl TestDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn test_dir() -> TestDir {
        let path =
            std::env::temp_dir().join(format!("llmux-account-history-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&path).unwrap();
        TestDir(path)
    }
    #[test]
    fn retains_failures_and_recovery_without_unbounded_poll_rows() {
        let store = AccountHistory::default();
        for _ in 0..200 {
            store.record(AccountEvent::new(
                "a",
                "usage_poll",
                "failed",
                Some(429),
                "rate limited",
                "auth unchanged",
                UNIX_EPOCH,
            ));
        }
        store.record(AccountEvent::new(
            "a",
            "oauth_refresh",
            "failed",
            Some(400),
            "invalid_grant",
            "auth_failed",
            UNIX_EPOCH,
        ));
        store.record(AccountEvent::new(
            "a",
            "registration",
            "success",
            None,
            "new credentials installed",
            "healthy; usage due",
            UNIX_EPOCH,
        ));
        let rows = store.snapshot();
        assert_eq!(rows["a"].len(), 3);
        assert_eq!(rows["a"][2].count, 200);
        for i in 0..100 {
            store.record(AccountEvent::new(
                "a",
                "usage_poll",
                "failed",
                Some(i),
                "different",
                "unchanged",
                UNIX_EPOCH,
            ));
        }
        assert_eq!(store.snapshot()["a"].len(), PER_ACCOUNT_LIMIT);
    }
    #[test]
    fn persisted_history_roundtrips_private_and_bounded() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir();
        let path = dir.path().join("account-history.json");
        let event = AccountEvent::new(
            "a",
            "oauth_refresh",
            "failed",
            Some(400),
            "invalid_grant: refresh token expired",
            "auth_failed",
            UNIX_EPOCH,
        );
        let events = VecDeque::from([event]);
        write_history(&path, &events).unwrap();
        assert_eq!(read_history(&path).unwrap(), events);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[test]
    fn persisted_unicode_history_fits_replay_limit_and_keeps_newest() {
        let dir = test_dir();
        let path = dir.path().join("account-history.json");
        let events: VecDeque<_> = (0..TOTAL_LIMIT)
            .map(|index| {
                AccountEvent::new(
                    &format!("{index}{}", "🦀".repeat(256)),
                    &"🦀".repeat(48),
                    &"🦀".repeat(32),
                    Some(400),
                    &"🦀".repeat(240),
                    &"🦀".repeat(96),
                    UNIX_EPOCH,
                )
            })
            .collect();
        write_history(&path, &events).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() <= FILE_LIMIT);
        let replayed = read_history(&path).unwrap();
        assert!(!replayed.is_empty());
        assert!(replayed.len() < events.len());
        assert_eq!(replayed.back(), events.back());
    }

    #[test]
    fn writer_keeps_latest_snapshot_after_burst_and_corrupt_history() {
        let dir = test_dir();
        let path = dir.path().join("account-history.json");
        std::fs::write(&path, b"not-json").unwrap();
        let store = AccountHistory::default();
        store.arm(path.clone()).unwrap();
        for i in 0..2000 {
            store.record(AccountEvent::new(
                "a",
                "usage_poll",
                "failed",
                Some(429),
                "rate limited",
                "auth unchanged",
                UNIX_EPOCH + std::time::Duration::from_millis(i),
            ));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if read_history(&path)
                .ok()
                .and_then(|events| events.back().cloned())
                .is_some_and(|e| e.count == 2000)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "latest pending receipt never reached disk"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    #[test]
    fn replay_masks_credentials_and_reused_temporary_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir();
        let path = dir.path().join("account-history.json");
        let mut event = AccountEvent::new(
            "a",
            "usage_poll",
            "failed",
            None,
            "safe",
            "unchanged",
            UNIX_EPOCH,
        );
        event.reason = "Bearer sk-ant-oat01-SECRET-DATA\nforged row".into();
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, b"old").unwrap();
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_history(&path, &VecDeque::from([event])).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let loaded = read_history(&path).unwrap();
        assert!(!loaded[0].reason.contains("SECRET-DATA"));
        assert!(!loaded[0].reason.contains('\n'));
    }

    #[test]
    fn oauth_history_never_copies_arbitrary_provider_text() {
        assert_eq!(
            oauth_reason(
                r#"{"error":"invalid_grant","error_description":"Refresh token expired"}"#
            ),
            "invalid_grant: refresh token expired"
        );
        let reason = oauth_reason(
            r#"{"error":"invalid_grant","error_description":"access_token=secret-unknown-format"}"#,
        );
        assert!(!reason.contains("secret"));
        assert!(reason.contains("undisclosed"));
    }
    #[test]
    fn dropping_store_flushes_last_pending_receipt() {
        let dir = test_dir();
        let path = dir.path().join("account-history.json");
        {
            let store = AccountHistory::default();
            store.arm(path.clone()).unwrap();
            for _ in 0..100 {
                store.record(AccountEvent::new(
                    "a",
                    "registration",
                    "success",
                    None,
                    "login installed",
                    "healthy",
                    UNIX_EPOCH,
                ));
            }
        }
        assert_eq!(read_history(&path).unwrap().back().unwrap().count, 100);
    }
}
