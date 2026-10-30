//! Day-partitioned raw-io store (raw-io v2): the SQL home of every
//! [`super::raw_io::RawIoRecord`], replacing the single append-only
//! `raw-io.jsonl` that grew to 0.6–1.7 TB per host.
//!
//! # Layout (`$XDG_STATE_HOME/llmux/store/raw-io/`)
//!
//! ```text
//! YYYY-MM-DD.sqlite3          hot partition, one per LOCAL calendar day
//! archive/YYYY-MM-DD[.N].jsonl.zst + .manifest.json
//!                             sealed past days (never deleted by llmux)
//! spool/current.jsonl         fallback when a partition write fails
//! spool/gen-*.jsonl           frozen spool generations awaiting import
//! legacy/                     fully imported legacy/spool files (kept)
//! sealing/ trash/             archive in progress / verified-archived hot DBs
//! import-state.json           resumable import checkpoints
//! ```
//!
//! # Row shape: metadata columns + the verbatim line
//!
//! Each row keeps the EXACT JSON line the old `append` would have written
//! (`line` BLOB) plus the metadata columns the readers filter on (ts, id,
//! model, account, user_id, tokens). Storage is lossless by construction: an
//! unparseable or partial legacy line is stored verbatim with `parsed = 0`.
//! The column set maps 1:1 onto a future server DBMS (Postgres: one
//! partitioned metadata table + a body column/blob store) — the reason this
//! module exposes a narrow API (insert / find / iterate / import / archive)
//! instead of SQLite filenames.
//!
//! # Why day archives are `jsonl.zst` with a long window
//!
//! Claude Code resends the whole conversation on every turn, so consecutive
//! records are mostly identical. Measured on 1 GiB of real raw-io: per-record
//! zstd -3 compresses 3.8x; zstd -3 with a long-distance window 72–77x. Only a
//! whole-day stream captures that, so past days are sealed into one
//! long-window zstd stream of their lines, in insertion order — byte-for-byte
//! the legacy `raw-io.jsonl` format, so `import` doubles as `restore`.
//!
//! # Nothing is ever deleted
//!
//! There is no retention: the archiver seals a day only after the compressed
//! stream decodes back to the same rows (count + per-row sha256), and the
//! sealed hot file is purged from `trash/` only after its archive decodes
//! again. Imports deduplicate by source occurrence (source identity + byte
//! offset), never by content, so two identical lines in a source stay two rows.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::raw_io::RawIoRecord;

/// Schema version recorded in each partition's `meta` table.
pub const SCHEMA_VERSION: u32 = 1;
/// zstd level for day archives (fast; the long window does the heavy lifting).
const ARCHIVE_LEVEL: i32 = 3;
/// Long-distance window: 2^30 = 1 GiB. Decode with `zstd -d --long=30`
/// (or any `--long=31`/`--memory=2048MB`).
const ARCHIVE_WINDOW_LOG: u32 = 30;
/// Import commit batch: whichever comes first.
const BATCH_LINES: usize = 512;
const BATCH_BYTES: usize = 64 * 1024 * 1024;
/// Lookup window around the activity entry's completion time (same as the
/// legacy jsonl scan).
pub const FIND_WINDOW_MS: u64 = 300_000;
const MS_PER_DAY: u64 = 86_400_000;
/// A sealed hot file stays in `trash/` at least this long.
const TRASH_MIN_AGE: Duration = Duration::from_secs(24 * 3600);
/// Default free-space floor for imports/restores (capacity admission).
pub const DEFAULT_MIN_FREE_BYTES: u64 = 20 * 1024 * 1024 * 1024;

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Partition holding lines that are not readable records (corrupt / torn
/// legacy lines), kept verbatim. A fixed key makes their placement
/// independent of import resume state.
pub const UNPARSED_DAY: &str = "1970-01-01";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("low disk: {free} bytes free < floor {floor} bytes")]
    LowDisk { free: u64, floor: u64 },
    #[error("archive verification failed for {day}: {detail}")]
    Verify { day: String, detail: String },
    /// Not now (another writer holds the day, or shutdown): retried next
    /// cycle, nothing failed.
    #[error("deferred: {0}")]
    Busy(String),
    #[error("{0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// Day keys (local calendar date)
// ---------------------------------------------------------------------------

/// Local calendar day (`YYYY-MM-DD`) of an epoch-ms timestamp, at the host's
/// UTC offset at that instant (`localtime_r`). Lookups always check BOTH
/// partitions a ±window straddles, so a midnight boundary never hides a row.
pub fn day_of(ts_ms: u64) -> String {
    let at = UNIX_EPOCH + Duration::from_millis(ts_ms);
    let offset = crate::tui::format::local_offset_secs(at);
    day_at_offset(ts_ms, offset)
}

/// Pure core of [`day_of`] (unit-testable with explicit offsets).
pub fn day_at_offset(ts_ms: u64, offset_secs: i64) -> String {
    let secs = i64::try_from(ts_ms / 1000).unwrap_or(i64::MAX) + offset_secs;
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's days→civil algorithm (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn is_day(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Row metadata
// ---------------------------------------------------------------------------

/// Metadata columns derived from one line. `parsed = false` → the line is not
/// a readable [`RawIoRecord`]; it is still stored verbatim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineMeta {
    pub parsed: bool,
    pub ts_ms: u64,
    pub id: u64,
    pub group: Option<String>,
    pub model: Option<String>,
    pub account: Option<String>,
    pub status: Option<u16>,
    pub duration_ms: Option<u64>,
    pub user_id: Option<String>,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

impl LineMeta {
    pub fn from_record(rec: &RawIoRecord) -> Self {
        let (tokens_in, tokens_out) = crate::session::tokens_from_response_body(&rec.response_body);
        Self {
            parsed: true,
            ts_ms: rec.ts_ms,
            id: rec.id,
            group: rec.group.clone(),
            model: rec.model.clone(),
            account: rec.account.clone(),
            status: rec.status,
            duration_ms: rec.duration_ms,
            user_id: crate::session::user_id_from_request_body(&rec.request_body),
            tokens_in,
            tokens_out,
        }
    }

    /// Parse a stored/legacy line. Unparseable → `parsed = false`, `ts_ms = 0`
    /// (such rows live in the fixed [`UNPARSED_DAY`] partition, so a re-read
    /// after a resume always lands in the same partition and deduplicates).
    pub fn from_line(line: &[u8]) -> Self {
        match serde_json::from_slice::<RawIoRecord>(line) {
            Ok(rec) => Self::from_record(&rec),
            Err(_) => Self::default(),
        }
    }
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn i64_of(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn u64_of(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Partitions
// ---------------------------------------------------------------------------

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS raw_io (
    seq         INTEGER PRIMARY KEY,
    ts_ms       INTEGER NOT NULL,
    id          INTEGER NOT NULL,
    grp         TEXT,
    model       TEXT,
    account     TEXT,
    status      INTEGER,
    duration_ms INTEGER,
    user_id     TEXT,
    tokens_in   INTEGER NOT NULL DEFAULT 0,
    tokens_out  INTEGER NOT NULL DEFAULT 0,
    parsed      INTEGER NOT NULL,
    rec_hash    BLOB NOT NULL,
    src         TEXT,
    src_off     INTEGER,
    line        BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS raw_io_id_ts ON raw_io(id, ts_ms);
CREATE INDEX IF NOT EXISTS raw_io_ts ON raw_io(ts_ms);
CREATE UNIQUE INDEX IF NOT EXISTS raw_io_src ON raw_io(src, src_off);
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
";

/// How a partition connection syncs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sync {
    /// Live capture: `synchronous=FULL` — a committed row survives power loss.
    Full,
    /// Bulk import: `synchronous=FULL` too (one fsync per ≤64 MiB batch, so
    /// the checkpoint written after a commit can never run ahead of the
    /// rows), with a larger WAL autocheckpoint.
    Bulk,
}

/// Open (creating if needed) a partition read-write: WAL, 64 KiB pages (rows
/// average ~1.3 MB), the requested sync level.
fn open_partition(path: &Path, day: &str, sync: Sync) -> Result<Connection, StoreError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let fresh = !path.exists();
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(60))?;
    if fresh {
        conn.execute_batch("PRAGMA page_size = 65536;")?;
    }
    conn.execute_batch(match sync {
        Sync::Full => "PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;",
        Sync::Bulk => {
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL; PRAGMA wal_autocheckpoint = 16384;"
        }
    })?;
    conn.execute_batch(SCHEMA_SQL)?;
    conn.execute(
        "INSERT OR IGNORE INTO meta (k, v) VALUES ('schema_version', ?1), ('day', ?2)",
        params![SCHEMA_VERSION.to_string(), day],
    )?;
    Ok(conn)
}

fn open_readonly(path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(10))?;
    Ok(conn)
}

/// Insert one row. Returns `true` when inserted, `false` when the
/// `(src, src_off)` occurrence already exists (re-import / resume).
fn insert_row(
    conn: &Connection,
    meta: &LineMeta,
    line: &[u8],
    src: Option<&str>,
    src_off: Option<u64>,
) -> Result<bool, StoreError> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO raw_io
           (ts_ms, id, grp, model, account, status, duration_ms, user_id,
            tokens_in, tokens_out, parsed, rec_hash, src, src_off, line)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            i64_of(meta.ts_ms),
            i64_of(meta.id),
            meta.group,
            meta.model,
            meta.account,
            meta.status.map(i64::from),
            meta.duration_ms.map(i64_of),
            meta.user_id,
            i64_of(meta.tokens_in),
            i64_of(meta.tokens_out),
            i64::from(meta.parsed),
            sha256(line),
            src,
            src_off.map(i64_of),
            line,
        ],
    )?;
    Ok(n == 1)
}

// ---------------------------------------------------------------------------
// Manifests / import state
// ---------------------------------------------------------------------------

/// Written next to each archive generation once it has been verified.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveManifest {
    pub day: String,
    pub generation: u32,
    pub rows: u64,
    pub plain_bytes: u64,
    pub plain_sha256: String,
    pub archive_bytes: u64,
    /// Rows per source (`"live"` for captured traffic, else the import source
    /// identity) — lets import reconciliation count archived rows.
    pub src_counts: BTreeMap<String, u64>,
    pub zstd: String,
    pub created_ms: u64,
}

/// One contiguous imported byte range of a source with its sha256 — matchable
/// against an external backup of the same bytes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Segment {
    pub start: u64,
    pub end: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceState {
    pub path: String,
    /// Next byte offset to import (everything before it is committed).
    pub offset: u64,
    pub lines: u64,
    pub inserted: u64,
    pub duplicates: u64,
    pub unparsed: u64,
    pub blank: u64,
    pub segments: Vec<Segment>,
    pub completed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ImportState {
    sources: BTreeMap<String, SourceState>,
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    sync_dir(path.parent());
    Ok(())
}

fn sync_dir(dir: Option<&Path>) {
    if let Some(dir) = dir {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
}

/// Free bytes available to this user on the volume holding `path`.
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt as _;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs fills the zeroed struct; we only read it on success.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)]
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

#[cfg(not(unix))]
pub fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

/// Stable identity of an import source: device + inode + sha256 of the first
/// 4 MiB. A rename/move keeps the identity; a decompressed backup of the same
/// file shares its first bytes (→ same identity, same offsets).
fn source_identity_of_file(path: &Path) -> Result<String, StoreError> {
    let mut f = File::open(path)?;
    let mut head = vec![0u8; 4 * 1024 * 1024];
    let n = read_full(&mut f, &mut head)?;
    head.truncate(n);
    Ok(format!("sha256-4m:{}", hex(&sha256(&head))))
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let k = r.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// Outcome of a lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Found(Box<RawIoRecord>),
    /// Not in any hot partition, but the day is archived — restore with
    /// `llmux raw-io restore DAY`.
    Archived(String),
    NotFound,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ImportOutcome {
    pub source: String,
    pub lines: u64,
    pub inserted: u64,
    pub duplicates: u64,
    pub unparsed: u64,
    pub blank: u64,
    pub filtered: u64,
    pub bytes: u64,
    /// sha256 of the bytes read this run (all of them, including filtered).
    pub sha256: String,
    pub days: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    pub from_day: Option<String>,
    pub to_day: Option<String>,
    pub min_free_bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArchiveOutcome {
    pub day: String,
    pub generation: u32,
    pub rows: u64,
    pub plain_bytes: u64,
    pub archive_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PartitionInfo {
    pub day: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StoreStatus {
    pub root: String,
    pub hot: Vec<PartitionInfo>,
    pub archives: Vec<ArchiveManifest>,
    pub sealing: Vec<String>,
    pub trash: Vec<String>,
    pub spool_files: Vec<String>,
    pub legacy_files: Vec<String>,
    pub pending_legacy: Option<String>,
    pub imports: BTreeMap<String, SourceState>,
    pub spooled: u64,
    pub lost: u64,
    pub free_bytes: Option<u64>,
}

struct Writer {
    day: String,
    conn: Connection,
    /// Shared lock on the day's lock file, held for the connection's
    /// lifetime: a sealer in ANY process needs it exclusively, so it can never
    /// move a partition another daemon (e.g. a predecessor still draining in
    /// a rolling restart) is writing to.
    _day_lock: File,
}

/// Process-wide registry: one store per root, so every capture call site that
/// knows only the legacy `raw-io.jsonl` path shares one writer.
fn registry() -> &'static Mutex<BTreeMap<PathBuf, Arc<RawStore>>> {
    static REG: OnceLock<Mutex<BTreeMap<PathBuf, Arc<RawStore>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The store anchored at a legacy `raw-io.jsonl` path:
/// `<dir>/raw-io.jsonl` → root `<dir>/store/raw-io/`.
pub fn store_for(raw_io_path: &Path) -> Arc<RawStore> {
    let mut reg = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    reg.entry(raw_io_path.to_path_buf())
        .or_insert_with(|| Arc::new(RawStore::new(raw_io_path)))
        .clone()
}

pub struct RawStore {
    root: PathBuf,
    legacy: PathBuf,
    writer: Mutex<Option<Writer>>,
    spool_lock: Mutex<()>,
    spooled: AtomicU64,
    lost: AtomicU64,
    stop: AtomicBool,
}

impl RawStore {
    pub fn new(raw_io_path: &Path) -> Self {
        let dir = raw_io_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::with_root(dir.join("store").join("raw-io"), raw_io_path.to_path_buf())
    }

    /// Store with an explicit root (CLI `--store`); `legacy` is the
    /// pre-upgrade jsonl auto-imported by [`RawStore::import_pending`].
    pub fn with_root(root: PathBuf, legacy: PathBuf) -> Self {
        Self {
            root,
            legacy,
            writer: Mutex::new(None),
            spool_lock: Mutex::new(()),
            spooled: AtomicU64::new(0),
            lost: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        }
    }

    /// Ask a running import to stop at its next committed batch (daemon
    /// shutdown). Imports are resumable, so stopping loses nothing.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    fn hot_path(&self, day: &str) -> PathBuf {
        self.root.join(format!("{day}.sqlite3"))
    }
    fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    /// `<root>/.locks/DAY.lock` — writers hold it shared, sealing exclusive.
    fn day_lock_file(&self, day: &str) -> Result<File, StoreError> {
        let dir = self.root.join(".locks");
        fs::create_dir_all(&dir)?;
        Ok(OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{day}.lock")))?)
    }

    fn maint_lock(&self) -> Result<File, StoreError> {
        fs::create_dir_all(&self.root)?;
        Ok(OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join(".maint.lock"))?)
    }

    // ---- live capture -----------------------------------------------------

    /// Persist one captured record. On any partition error the line goes to
    /// the fsynced spool (auto-imported later); if the spool fails too the
    /// loss is counted and logged at error level — the request itself is
    /// never failed (capture is observe-only).
    pub fn capture(&self, record: &RawIoRecord) {
        let Ok(line) = serde_json::to_string(record) else {
            self.lost.fetch_add(1, Ordering::Relaxed);
            tracing::error!(
                id = record.id,
                "raw-io record serialization failed — record lost"
            );
            return;
        };
        if let Err(err) = self.insert_live(line.as_bytes(), record) {
            tracing::warn!(error = %err, id = record.id, "raw-io partition write failed — spooling");
            self.spool(line.as_bytes());
        }
    }

    fn insert_live(&self, line: &[u8], record: &RawIoRecord) -> Result<(), StoreError> {
        let meta = LineMeta::from_record(record);
        let day = day_of(record.ts_ms);
        let mut guard = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.as_ref().is_none_or(|w| w.day != day) {
            *guard = None; // close the previous day before opening the next
                           // Lock BEFORE opening the file. A day being sealed right now
                           // (exclusive holder) makes this write go to the spool instead of
                           // blocking captures; the spool lands in a fresh partition later.
            let day_lock = self.day_lock_file(&day)?;
            match day_lock.try_lock_shared() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(StoreError::Other(format!("{day} is being sealed")));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
            }
            let conn = open_partition(&self.hot_path(&day), &day, Sync::Full)?;
            *guard = Some(Writer {
                day,
                conn,
                _day_lock: day_lock,
            });
        }
        let Some(w) = guard.as_ref() else {
            return Err(StoreError::Other("writer unavailable".into()));
        };
        insert_row(&w.conn, &meta, line, None, None)?;
        Ok(())
    }

    fn spool(&self, line: &[u8]) {
        let _g = self
            .spool_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.dir("spool").join("current.jsonl");
        let res = (|| -> std::io::Result<()> {
            fs::create_dir_all(self.dir("spool"))?;
            let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
            f.write_all(line)?;
            f.write_all(b"\n")?;
            f.sync_data()
        })();
        match res {
            Ok(()) => {
                self.spooled.fetch_add(1, Ordering::Relaxed);
            }
            Err(err) => {
                self.lost.fetch_add(1, Ordering::Relaxed);
                tracing::error!(error = %err, path = %path.display(), "raw-io spool write failed — record lost");
            }
        }
    }

    /// `(spooled, lost)` counters since process start.
    pub fn counters(&self) -> (u64, u64) {
        (
            self.spooled.load(Ordering::Relaxed),
            self.lost.load(Ordering::Relaxed),
        )
    }

    // ---- lookup -----------------------------------------------------------

    /// Find activity `id` completed around `at_ms` (closest capture time
    /// within ±[`FIND_WINDOW_MS`]) in the hot (and sealing) partitions of the
    /// day(s) the window touches.
    pub fn find(&self, id: u64, at_ms: u64) -> Lookup {
        let lo = at_ms.saturating_sub(FIND_WINDOW_MS);
        let hi = at_ms.saturating_add(FIND_WINDOW_MS);
        let mut days = vec![day_of(lo), day_of(hi)];
        days.dedup();
        let mut best: Option<(u64, Vec<u8>)> = None;
        let mut archived: Option<String> = None;
        for day in &days {
            for path in [
                self.hot_path(day),
                self.dir("sealing").join(format!("{day}.sqlite3")),
            ] {
                if !path.exists() {
                    continue;
                }
                let Ok(conn) = open_readonly(&path) else {
                    continue;
                };
                let row = conn.query_row(
                    "SELECT ts_ms, line FROM raw_io
                      WHERE id = ?1 AND ts_ms BETWEEN ?2 AND ?3 AND parsed = 1
                      ORDER BY abs(ts_ms - ?4) LIMIT 1",
                    params![i64_of(id), i64_of(lo), i64_of(hi), i64_of(at_ms)],
                    |r| Ok((u64_of(r.get::<_, i64>(0)?), r.get::<_, Vec<u8>>(1)?)),
                );
                if let Ok((ts, line)) = row {
                    let dist = ts.abs_diff(at_ms);
                    if best.as_ref().is_none_or(|(d, _)| dist < *d) {
                        best = Some((dist, line));
                    }
                }
            }
            // Archived even when a later-generation hot partition exists:
            // the record may sit in an earlier generation.
            if !self.archives_of(day).is_empty() {
                archived = Some(day.clone());
            }
        }
        if let Some((_, line)) = best {
            if let Ok(rec) = serde_json::from_slice::<RawIoRecord>(&line) {
                return Lookup::Found(Box::new(rec));
            }
        }
        match archived {
            Some(day) => Lookup::Archived(day),
            None => Lookup::NotFound,
        }
    }

    // ---- iteration ----------------------------------------------------------

    /// Hot partition days, ascending.
    pub fn hot_days(&self) -> Vec<String> {
        let mut days: Vec<String> = fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let day = name.strip_suffix(".sqlite3")?.to_string();
                is_day(&day).then_some(day)
            })
            .collect();
        days.sort();
        days
    }

    /// Every parseable record in the hot partitions (insertion order per day).
    /// Test/diagnostic helper — production readers use the metadata columns.
    pub fn load_hot_records(&self) -> Vec<RawIoRecord> {
        let mut out = Vec::new();
        for day in self.hot_days() {
            let Ok(conn) = open_readonly(&self.hot_path(&day)) else {
                continue;
            };
            let Ok(mut stmt) =
                conn.prepare("SELECT line FROM raw_io WHERE parsed = 1 ORDER BY seq")
            else {
                continue;
            };
            let Ok(rows) = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0)) else {
                continue;
            };
            out.extend(
                rows.flatten()
                    .filter_map(|l| serde_json::from_slice::<RawIoRecord>(&l).ok()),
            );
        }
        out
    }

    /// Session-fold rows (metadata only — no bodies are read) from every hot
    /// partition, newest day first; `on_day` receives each day's rows and
    /// returns `false` to stop. `live_only` skips imported rows (used while
    /// the legacy file they came from is still read directly).
    pub fn for_each_session_rows(
        &self,
        live_only: bool,
        mut on_day: impl FnMut(Vec<crate::session::SessionRow>) -> bool,
    ) {
        let mut days = self.hot_days();
        days.reverse();
        for day in days {
            let Ok(conn) = open_readonly(&self.hot_path(&day)) else {
                continue;
            };
            let sql = if live_only {
                "SELECT ts_ms, id, model, account, duration_ms, user_id, tokens_in, tokens_out
                   FROM raw_io WHERE parsed = 1 AND src IS NULL"
            } else {
                "SELECT ts_ms, id, model, account, duration_ms, user_id, tokens_in, tokens_out
                   FROM raw_io WHERE parsed = 1"
            };
            let Ok(mut stmt) = conn.prepare(sql) else {
                continue;
            };
            let Ok(rows) = stmt.query_map([], |r| {
                Ok(crate::session::SessionRow {
                    ts_ms: u64_of(r.get(0)?),
                    id: u64_of(r.get(1)?),
                    model: r.get(2)?,
                    account: r.get(3)?,
                    duration_ms: r.get::<_, Option<i64>>(4)?.map(u64_of),
                    user_id: r.get(5)?,
                    tokens_in: u64_of(r.get(6)?),
                    tokens_out: u64_of(r.get(7)?),
                })
            }) else {
                continue;
            };
            if !on_day(rows.flatten().collect()) {
                return;
            }
        }
    }

    // ---- archives -------------------------------------------------------------

    /// `(generation, path)` of every archive of `day`, ascending.
    pub fn archives_of(&self, day: &str) -> Vec<(u32, PathBuf)> {
        let dir = self.dir("archive");
        let mut out: Vec<(u32, PathBuf)> = fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let rest = name.strip_prefix(day)?.strip_suffix(".jsonl.zst")?;
                let generation = if rest.is_empty() {
                    0
                } else {
                    rest.strip_prefix('.')?.parse().ok()?
                };
                Some((generation, e.path()))
            })
            .collect();
        out.sort();
        out
    }

    fn archive_name(day: &str, generation: u32) -> String {
        if generation == 0 {
            format!("{day}.jsonl.zst")
        } else {
            format!("{day}.{generation}.jsonl.zst")
        }
    }

    fn manifest_path(&self, day: &str, generation: u32) -> PathBuf {
        let name = Self::archive_name(day, generation);
        self.dir("archive")
            .join(name.replace(".jsonl.zst", ".manifest.json"))
    }

    fn read_manifest(&self, day: &str, generation: u32) -> Option<ArchiveManifest> {
        let text = fs::read_to_string(self.manifest_path(day, generation)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Seal every eligible past day: days older than `hot_days` full days
    /// before today (never today, unless `include_today` — the low-disk
    /// escape hatch). Skips the whole cycle when an import/restore holds the
    /// maintenance lock. Crash leftovers in `sealing/` are finished first.
    pub fn archive_cycle(
        &self,
        now_ms: u64,
        hot_days: u64,
        include_today: bool,
    ) -> Result<Vec<ArchiveOutcome>, StoreError> {
        let lock = self.maint_lock()?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(Vec::new()),
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
        let mut out = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        // 1. Crash leftovers: a sealed hot DB is the only copy until archived.
        for day in self.sealing_days() {
            if self.stop_requested() {
                break;
            }
            match self.seal_and_archive(&day, false) {
                Ok(o) => out.push(o),
                Err(StoreError::Busy(why)) => tracing::info!(day, why, "raw-io seal deferred"),
                Err(err) => {
                    tracing::error!(day, error = %err, "raw-io seal failed — data kept in sealing/");
                    failures.push(format!("{day}: {err}"));
                }
            }
        }
        // 2. Eligible hot days.
        let today = day_of(now_ms);
        let cutoff = day_of(now_ms.saturating_sub(hot_days.saturating_mul(MS_PER_DAY)));
        for day in self.hot_days() {
            let eligible = if include_today {
                true
            } else {
                day < cutoff && day < today
            };
            if !eligible || self.stop_requested() {
                continue;
            }
            // One bad day must not wedge the others (or the trash purge).
            match self.seal_and_archive(&day, true) {
                Ok(o) => out.push(o),
                Err(StoreError::Busy(why)) => tracing::info!(day, why, "raw-io seal deferred"),
                Err(err) => {
                    tracing::error!(day, error = %err, "raw-io seal failed — data kept");
                    failures.push(format!("{day}: {err}"));
                }
            }
        }
        // On low disk (`include_today`) a re-verified trash entry goes now
        // instead of after the minimum age.
        self.purge_trash(include_today);
        drop(lock);
        if !failures.is_empty() && out.is_empty() {
            return Err(StoreError::Other(format!(
                "archive failed: {}",
                failures.join("; ")
            )));
        }
        Ok(out)
    }

    fn sealing_days(&self) -> Vec<String> {
        let mut days: Vec<String> = fs::read_dir(self.dir("sealing"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let day = name.strip_suffix(".sqlite3")?.to_string();
                is_day(&day).then_some(day)
            })
            .collect();
        days.sort();
        days
    }

    /// Move the hot file of `day` into `sealing/` under the writer lock (the
    /// in-process writer is closed first if it holds that day; any later write
    /// for the day opens a FRESH partition → a later archive generation), then
    /// archive + verify it, then move it to `trash/`.
    fn seal_and_archive(
        &self,
        day: &str,
        move_from_hot: bool,
    ) -> Result<ArchiveOutcome, StoreError> {
        let sealing_dir = self.dir("sealing");
        fs::create_dir_all(&sealing_dir)?;
        let sealed = sealing_dir.join(format!("{day}.sqlite3"));
        if move_from_hot {
            let mut guard = self
                .writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if sealed.exists() {
                return Err(StoreError::Other(format!(
                    "{day}: a sealing copy already exists — finish it first"
                )));
            }
            let hot = self.hot_path(day);
            if guard.as_ref().is_some_and(|w| w.day == day) {
                // Our own writer: checkpoint, then close (releases its
                // shared day lock).
                if let Some(w) = guard.as_ref() {
                    w.conn
                        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
                }
                *guard = None;
            }
            // No writer in any process may hold the day: exclusive or skip.
            let day_lock = self.day_lock_file(day)?;
            match day_lock.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(StoreError::Busy(format!(
                        "{day} is open for writing in another process — not sealing this cycle"
                    )));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
            }
            // Fold the WAL into the main file BEFORE anything moves, so no
            // committed frame lives only in a sidecar (a sidecar left behind
            // could otherwise be replayed into a fresh partition). The writer
            // lock is held, so no live capture can reopen the day meanwhile;
            // importers are excluded by the exclusive maintenance lock.
            {
                let conn = Connection::open(&hot)?;
                conn.busy_timeout(Duration::from_secs(60))?;
                conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            }
            let wal = PathBuf::from(format!("{}-wal", hot.display()));
            if fs::metadata(&wal).map(|m| m.len()).unwrap_or(0) > 0 {
                return Err(StoreError::Other(format!(
                    "{day}: WAL still holds frames after checkpoint (another connection?) — not sealing"
                )));
            }
            for suffix in ["-wal", "-shm"] {
                let side = PathBuf::from(format!("{}{suffix}", hot.display()));
                if side.exists() {
                    fs::rename(
                        &side,
                        PathBuf::from(format!("{}{suffix}", sealed.display())),
                    )?;
                }
            }
            fs::rename(&hot, &sealed)?;
            sync_dir(Some(&self.root));
            sync_dir(Some(&sealing_dir));
            drop(day_lock);
            drop(guard);
        }

        // Fold any WAL into the main file and sanity-check the database.
        let conn = Connection::open(&sealed)?;
        conn.busy_timeout(Duration::from_secs(60))?;
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(StoreError::Verify {
                day: day.into(),
                detail: format!("quick_check: {check}"),
            });
        }
        let rows: u64 = u64_of(conn.query_row("SELECT count(*) FROM raw_io", [], |r| r.get(0))?);
        let mut src_counts = BTreeMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT coalesce(src, 'live'), count(*) FROM raw_io GROUP BY 1")?;
            let it = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, u64_of(r.get(1)?))))?;
            for kv in it {
                let (k, v) = kv?;
                src_counts.insert(k, v);
            }
        }

        // Write the archive generation.
        let archive_dir = self.dir("archive");
        fs::create_dir_all(&archive_dir)?;
        let generation = self
            .archives_of(day)
            .last()
            .map(|(g, _)| g + 1)
            .unwrap_or(0);
        let final_path = archive_dir.join(Self::archive_name(day, generation));
        let tmp = PathBuf::from(format!("{}.tmp", final_path.display()));
        let (plain_bytes, plain_sha) = {
            let file = File::create(&tmp)?;
            let mut enc = zstd::stream::write::Encoder::new(file, ARCHIVE_LEVEL)?;
            enc.long_distance_matching(true)?;
            enc.window_log(ARCHIVE_WINDOW_LOG)?;
            enc.include_checksum(true)?;
            let mut hasher = Sha256::new();
            let mut bytes = 0u64;
            let mut stmt = conn.prepare("SELECT line FROM raw_io ORDER BY seq")?;
            let mut it = stmt.query([])?;
            let mut n = 0u64;
            while let Some(row) = it.next()? {
                n += 1;
                if n.is_multiple_of(64) && self.stop_requested() {
                    drop(enc);
                    let _ = fs::remove_file(&tmp);
                    return Err(StoreError::Busy(format!(
                        "{day}: shutdown during archive — sealed copy kept in sealing/"
                    )));
                }
                let line: Vec<u8> = row.get(0)?;
                enc.write_all(&line)?;
                enc.write_all(b"\n")?;
                hasher.update(&line);
                hasher.update(b"\n");
                bytes += line.len() as u64 + 1;
            }
            let file = enc.finish()?;
            file.sync_all()?;
            (bytes, hex(&hasher.finalize()))
        };

        // Verify: decode, compare row count + every row's sha256 in order.
        {
            let mut stmt = conn.prepare("SELECT rec_hash FROM raw_io ORDER BY seq")?;
            let mut hashes = stmt.query([])?;
            let mut decoder = zstd::stream::read::Decoder::new(File::open(&tmp)?)?;
            decoder.window_log_max(31)?;
            let mut reader = BufReader::with_capacity(1 << 20, decoder);
            let mut buf = Vec::new();
            let mut n = 0u64;
            loop {
                buf.clear();
                let k = reader.read_until(b'\n', &mut buf)?;
                if k == 0 {
                    break;
                }
                if n % 64 == 63 && self.stop_requested() {
                    let _ = fs::remove_file(&tmp);
                    return Err(StoreError::Busy(format!(
                        "{day}: shutdown during verify — sealed copy kept in sealing/"
                    )));
                }
                if buf.last() == Some(&b'\n') {
                    buf.pop();
                }
                let Some(row) = hashes.next()? else {
                    let _ = fs::remove_file(&tmp);
                    return Err(StoreError::Verify {
                        day: day.into(),
                        detail: "archive has more lines than rows".into(),
                    });
                };
                let want: Vec<u8> = row.get(0)?;
                if sha256(&buf) != want {
                    let _ = fs::remove_file(&tmp);
                    return Err(StoreError::Verify {
                        day: day.into(),
                        detail: format!("row {n} hash mismatch"),
                    });
                }
                n += 1;
            }
            if hashes.next()?.is_some() || n != rows {
                let _ = fs::remove_file(&tmp);
                return Err(StoreError::Verify {
                    day: day.into(),
                    detail: format!("decoded {n} lines, expected {rows} rows"),
                });
            }
        }
        drop(conn);

        // A crash between publish and the trash move re-runs this function on
        // the same sealed DB: an identical earlier generation is enough.
        let already = self.archives_of(day).into_iter().any(|(g, _)| {
            self.read_manifest(day, g)
                .is_some_and(|m| m.plain_sha256 == plain_sha && m.rows == rows)
        });
        let archive_bytes = fs::metadata(&tmp)?.len();
        let generation_used = if already {
            let _ = fs::remove_file(&tmp);
            self.archives_of(day)
                .into_iter()
                .find(|(g, _)| {
                    self.read_manifest(day, *g)
                        .is_some_and(|m| m.plain_sha256 == plain_sha)
                })
                .map(|(g, _)| g)
                .unwrap_or(generation)
        } else {
            fs::rename(&tmp, &final_path)?;
            sync_dir(Some(&archive_dir));
            let manifest = ArchiveManifest {
                day: day.into(),
                generation,
                rows,
                plain_bytes,
                plain_sha256: plain_sha,
                archive_bytes,
                src_counts,
                zstd: format!("level {ARCHIVE_LEVEL}, long window 2^{ARCHIVE_WINDOW_LOG}; decode: zstd -d --long=31"),
                created_ms: super::raw_io::now_ms(),
            };
            write_json_atomic(&self.manifest_path(day, generation), &manifest)?;
            generation
        };

        // The sealed hot DB moves to trash (purged later, only after the
        // archive decodes again).
        let trash = self.dir("trash");
        fs::create_dir_all(&trash)?;
        let stamp = super::raw_io::now_ms();
        for suffix in ["", "-wal", "-shm"] {
            let from = PathBuf::from(format!("{}{suffix}", sealed.display()));
            if from.exists() {
                let to = trash.join(format!("{day}.g{generation_used}.t{stamp}.sqlite3{suffix}"));
                fs::rename(&from, &to)?;
            }
        }
        sync_dir(Some(&trash));
        tracing::info!(
            day,
            generation = generation_used,
            rows,
            plain_bytes,
            archive_bytes,
            "raw-io day archived"
        );
        Ok(ArchiveOutcome {
            day: day.into(),
            generation: generation_used,
            rows,
            plain_bytes,
            archive_bytes,
        })
    }

    /// Unlink trash entries older than [`TRASH_MIN_AGE`] (any age when
    /// `low_disk`) whose archive generation still decodes to the manifest's
    /// row count.
    fn purge_trash(&self, low_disk: bool) {
        let trash = self.dir("trash");
        let Ok(entries) = fs::read_dir(&trash) else {
            return;
        };
        for e in entries.flatten() {
            if self.stop_requested() {
                return;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let Some(stem) = name.strip_suffix(".sqlite3") else {
                continue;
            };
            // `DAY.g<gen>.t<trashed-at-ms>`: rename keeps the old mtime, so
            // the trash time travels in the name.
            let Some((day, rest)) = stem.split_once(".g") else {
                continue;
            };
            let Some((gen, at)) = rest.split_once(".t") else {
                continue;
            };
            let (Ok(generation), Ok(at_ms)) = (gen.parse::<u32>(), at.parse::<u64>()) else {
                continue;
            };
            let old_enough = super::raw_io::now_ms().saturating_sub(at_ms)
                >= u64::try_from(TRASH_MIN_AGE.as_millis()).unwrap_or(u64::MAX);
            if !old_enough && !low_disk {
                continue;
            }
            let Some(manifest) = self.read_manifest(day, generation) else {
                continue;
            };
            let archive = self
                .dir("archive")
                .join(Self::archive_name(day, generation));
            match count_lines_zst(&archive) {
                Ok(n) if n == manifest.rows => {
                    for suffix in ["", "-wal", "-shm"] {
                        let _ = fs::remove_file(trash.join(format!("{name}{suffix}")));
                    }
                    tracing::info!(day, generation, "raw-io trash purged (archive re-verified)");
                }
                other => {
                    tracing::warn!(
                        day,
                        generation,
                        ?other,
                        "raw-io trash kept: archive did not re-verify"
                    );
                }
            }
        }
    }

    // ---- import -----------------------------------------------------------------

    /// Import a jsonl or jsonl.zst file (e.g. a backup, a day archive) under a
    /// shared maintenance lock. Occurrence-deduplicated: re-running the same
    /// source inserts nothing twice. Date filters only skip lines; they never
    /// mark the skipped range imported anywhere.
    pub fn import_file(
        &self,
        path: &Path,
        opts: &ImportOptions,
    ) -> Result<ImportOutcome, StoreError> {
        self.import_files(&[path.to_path_buf()], opts)
    }

    /// Import several files as ONE concatenated stream (e.g. a backup split
    /// into byte ranges `0-N` + `N-end`: offsets and the source identity then
    /// match the original file). Each file may be plain or zstd.
    pub fn import_files(
        &self,
        paths: &[PathBuf],
        opts: &ImportOptions,
    ) -> Result<ImportOutcome, StoreError> {
        let lock = self.maint_lock()?;
        lock.lock_shared()?;
        let mut chain: Box<dyn Read> = Box::new(std::io::empty());
        for path in paths {
            chain = Box::new(chain.chain(open_maybe_zstd(path)?));
        }
        // Identity = sha256 of the first 4 MiB of the decoded stream — the
        // same value `source_identity_of_file` gives the original file.
        let mut head = vec![0u8; 4 * 1024 * 1024];
        let n = read_full(&mut chain, &mut head)?;
        head.truncate(n);
        let identity = format!("sha256-4m:{}", hex(&sha256(&head)));
        let stream = std::io::Cursor::new(head).chain(chain);
        let mut reader = BufReader::with_capacity(4 << 20, stream);
        let outcome =
            self.import_reader(&mut reader, &identity, 0, opts, false, &mut |_, _| Ok(()))?;
        drop(lock);
        Ok(outcome)
    }

    /// Core import loop. `start_off` is the source byte offset of the first
    /// byte `reader` yields. `live_spool` → a trailing line without `\n` is
    /// left for the next run (still being appended); otherwise it is imported
    /// as a (torn) line. `checkpoint(next_offset, outcome)` runs after every
    /// committed batch.
    #[allow(clippy::too_many_lines)]
    fn import_reader(
        &self,
        reader: &mut dyn BufRead,
        src: &str,
        start_off: u64,
        opts: &ImportOptions,
        live_spool: bool,
        checkpoint: &mut dyn FnMut(u64, &ImportOutcome) -> Result<(), StoreError>,
    ) -> Result<ImportOutcome, StoreError> {
        let mut out = ImportOutcome {
            source: src.into(),
            ..ImportOutcome::default()
        };
        let mut hasher = Sha256::new();
        let mut off = start_off;
        let mut committed_off = start_off;
        let mut cur: Option<(String, Connection)> = None;
        let mut batch_lines = 0usize;
        let mut batch_bytes = 0usize;
        let mut buf = Vec::with_capacity(4 << 20);

        let commit = |cur: &mut Option<(String, Connection)>| -> Result<(), StoreError> {
            if let Some((_, conn)) = cur.as_ref() {
                if !conn.is_autocommit() {
                    conn.execute_batch("COMMIT")?;
                }
            }
            Ok(())
        };

        loop {
            buf.clear();
            let k = reader.read_until(b'\n', &mut buf)?;
            if k == 0 {
                break;
            }
            let complete = buf.last() == Some(&b'\n');
            if !complete && live_spool {
                break; // torn tail of a file still being written — next run
            }
            hasher.update(&buf);
            let line_off = off;
            off += k as u64;
            out.bytes += k as u64;
            let line: &[u8] = if complete {
                &buf[..buf.len() - 1]
            } else {
                &buf[..]
            };
            if line.is_empty() {
                out.blank += 1;
                continue;
            }
            out.lines += 1;
            let meta = LineMeta::from_line(line);
            let day = if meta.parsed {
                day_of(meta.ts_ms)
            } else {
                UNPARSED_DAY.to_string()
            };
            // Date filters select parsed records; unreadable lines carry no
            // date and are always kept.
            if meta.parsed
                && (opts.from_day.as_deref().is_some_and(|f| day.as_str() < f)
                    || opts.to_day.as_deref().is_some_and(|t| day.as_str() > t))
            {
                out.filtered += 1;
                continue;
            }
            if cur.as_ref().is_none_or(|(d, _)| d != &day) {
                commit(&mut cur)?;
                let conn = open_partition(&self.hot_path(&day), &day, Sync::Bulk)?;
                cur = Some((day.clone(), conn));
            }
            let Some((_, conn)) = cur.as_ref() else {
                return Err(StoreError::Other("partition unavailable".into()));
            };
            if conn.is_autocommit() {
                conn.execute_batch("BEGIN IMMEDIATE")?;
            }
            if insert_row(conn, &meta, line, Some(src), Some(line_off))? {
                out.inserted += 1;
                *out.days.entry(day).or_default() += 1;
                if !meta.parsed {
                    out.unparsed += 1;
                }
            } else {
                // Same occurrence already stored: it must be the same bytes,
                // otherwise two different sources share an identity.
                let existing: Vec<u8> = conn.query_row(
                    "SELECT rec_hash FROM raw_io WHERE src = ?1 AND src_off = ?2",
                    params![src, i64_of(line_off)],
                    |r| r.get(0),
                )?;
                if existing != sha256(line) {
                    return Err(StoreError::Verify {
                        day: day.clone(),
                        detail: format!(
                            "{src} offset {line_off}: stored bytes differ from the source — import stopped"
                        ),
                    });
                }
                out.duplicates += 1;
            }
            batch_lines += 1;
            batch_bytes += line.len();
            if batch_lines >= BATCH_LINES || batch_bytes >= BATCH_BYTES {
                commit(&mut cur)?;
                batch_lines = 0;
                batch_bytes = 0;
                committed_off = off;
                out.sha256 = hex(&hasher.clone().finalize());
                checkpoint(committed_off, &out)?;
                if self.stop_requested() {
                    return Ok(out);
                }
                if opts.min_free_bytes > 0 {
                    if let Some(free) = free_bytes(&self.root) {
                        if free < opts.min_free_bytes {
                            return Err(StoreError::LowDisk {
                                free,
                                floor: opts.min_free_bytes,
                            });
                        }
                    }
                }
            }
        }
        commit(&mut cur)?;
        drop(cur);
        out.sha256 = hex(&hasher.finalize());
        if off != committed_off || out.lines == 0 {
            checkpoint(off, &out)?;
        }
        Ok(out)
    }

    /// Daemon maintenance: freeze the spool into an immutable generation,
    /// then import the pre-upgrade legacy `raw-io.jsonl` and every spool
    /// generation, resumably. A fully imported, reconciled source is moved to
    /// `legacy/` (kept forever). Holds the shared maintenance lock.
    pub fn import_pending(
        &self,
        min_free_bytes: u64,
        include_legacy: bool,
    ) -> Result<Vec<ImportOutcome>, StoreError> {
        let lock = self.maint_lock()?;
        lock.lock_shared()?;
        self.rotate_spool()?;
        let mut sources: Vec<PathBuf> = Vec::new();
        if include_legacy && self.legacy.exists() {
            sources.push(self.legacy.clone());
        }
        let mut gens: Vec<PathBuf> = fs::read_dir(self.dir("spool"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("gen-") && n.ends_with(".jsonl"))
            })
            .collect();
        gens.sort();
        sources.extend(gens);
        let mut outcomes = Vec::new();
        for path in sources {
            outcomes.push(self.import_source_file(&path, min_free_bytes)?);
        }
        drop(lock);
        Ok(outcomes)
    }

    /// Rename `spool/current.jsonl` to an immutable `spool/gen-<ms>.jsonl`
    /// under the spool lock (appends after this go to a new current file).
    fn rotate_spool(&self) -> Result<(), StoreError> {
        let _g = self
            .spool_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cur = self.dir("spool").join("current.jsonl");
        if cur.exists() && fs::metadata(&cur)?.len() > 0 {
            let to = self
                .dir("spool")
                .join(format!("gen-{:013}.jsonl", super::raw_io::now_ms()));
            fs::rename(&cur, &to)?;
            sync_dir(Some(&self.dir("spool")));
        }
        Ok(())
    }

    fn load_import_state(&self) -> ImportState {
        fs::read_to_string(self.root.join("import-state.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save_import_state(&self, state: &ImportState) -> Result<(), StoreError> {
        write_json_atomic(&self.root.join("import-state.json"), state)
    }

    /// Resumable import of one immutable source file, then reconciliation:
    /// every non-blank line must be a row (hot partitions + archive
    /// manifests) before the file is moved to `legacy/`.
    fn import_source_file(
        &self,
        path: &Path,
        min_free_bytes: u64,
    ) -> Result<ImportOutcome, StoreError> {
        let identity = source_identity_of_file(path)?;
        let mut state = self.load_import_state();
        let base = {
            let e = state.sources.entry(identity.clone()).or_default();
            e.path = path.display().to_string();
            e.clone()
        };
        let start = base.offset;
        let len = fs::metadata(path)?.len();
        let mut f = File::open(path)?;
        std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(start))?;
        let mut reader = BufReader::with_capacity(4 << 20, f);
        let opts = ImportOptions {
            min_free_bytes,
            ..ImportOptions::default()
        };
        let outcome = {
            let mut ck = |next: u64, o: &ImportOutcome| -> Result<(), StoreError> {
                let e = state.sources.entry(identity.clone()).or_default();
                e.offset = next;
                e.lines = base.lines + o.lines;
                e.inserted = base.inserted + o.inserted;
                e.duplicates = base.duplicates + o.duplicates;
                e.unparsed = base.unparsed + o.unparsed;
                e.blank = base.blank + o.blank;
                let mut segs = base.segments.clone();
                if next > start {
                    segs.push(Segment {
                        start,
                        end: next,
                        sha256: o.sha256.clone(),
                    });
                }
                e.segments = segs;
                self.save_import_state(&state)
            };
            // Sources here are immutable (the legacy file is never appended
            // to by this binary; spool generations are frozen by rotation),
            // so a torn final line is imported, not waited for.
            self.import_reader(&mut reader, &identity, start, &opts, false, &mut ck)?
        };
        let mut state = self.load_import_state();
        let entry = state.sources.entry(identity.clone()).or_default();
        if entry.offset >= len && !entry.completed && !self.stop_requested() {
            // Reconcile before moving the source anywhere. `lines` is the
            // exact non-blank line count of [0, offset) (each checkpoint
            // stores offset and count together), and every line is one
            // occurrence = one row.
            let (hot_rows, archived_rows) = self.count_rows_of_source(&identity)?;
            let rows = hot_rows + archived_rows;
            let expected = entry.lines;
            if rows != expected && archived_rows > 0 {
                // Some of this source is already sealed into archives, which
                // carry no occurrence index: a re-pass would re-insert those
                // days and never converge. Keep the file and say so.
                return Err(StoreError::Verify {
                    day: "-".into(),
                    detail: format!(
                        "source {} has {expected} lines but {rows} rows ({archived_rows} archived) — kept in place; needs a manual check",
                        path.display()
                    ),
                });
            }
            if rows != expected {
                // Rows went missing while every imported row is still hot:
                // start the source over — occurrence dedupe makes the second
                // pass insert only what is missing. The source stays put.
                let shown = path.display().to_string();
                *entry = SourceState {
                    path: shown.clone(),
                    ..SourceState::default()
                };
                self.save_import_state(&state)?;
                return Err(StoreError::Verify {
                    day: "-".into(),
                    detail: format!(
                        "source {shown} has {expected} lines but {rows} rows — kept in place, re-importing from offset 0"
                    ),
                });
            }
            let legacy_dir = self.dir("legacy");
            fs::create_dir_all(&legacy_dir)?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "raw-io.jsonl".into());
            let to = legacy_dir.join(format!("{}.{name}", super::raw_io::now_ms()));
            fs::rename(path, &to)?;
            sync_dir(Some(&legacy_dir));
            entry.completed = true;
            entry.path = to.display().to_string();
            self.save_import_state(&state)?;
            tracing::info!(source = %to.display(), rows, "raw-io source fully imported and reconciled");
        }
        Ok(outcome)
    }

    /// Rows imported from `src`: `(hot + sealing rows, rows recorded in
    /// archive manifests)`. Any read error is an error — reconciliation
    /// must never act on an undercount.
    pub fn count_rows_of_source(&self, src: &str) -> Result<(u64, u64), StoreError> {
        let mut hot = 0u64;
        let mut paths: Vec<PathBuf> = self.hot_days().iter().map(|d| self.hot_path(d)).collect();
        // A sealing copy whose archive is already published (crash before the
        // trash move) is counted through its manifest, not twice.
        paths.extend(
            self.sealing_days()
                .iter()
                .filter(|d| self.archives_of(d).is_empty())
                .map(|d| self.dir("sealing").join(format!("{d}.sqlite3"))),
        );
        for p in paths {
            let conn = open_readonly(&p)?;
            let n: i64 =
                conn.query_row("SELECT count(*) FROM raw_io WHERE src = ?1", [src], |r| {
                    r.get(0)
                })?;
            hot += u64_of(n);
        }
        let mut archived = 0u64;
        let archive_dir = self.dir("archive");
        if archive_dir.exists() {
            for e in fs::read_dir(&archive_dir)? {
                let e = e?;
                let name = e.file_name().to_string_lossy().to_string();
                if name.ends_with(".manifest.json") {
                    let m: ArchiveManifest = serde_json::from_str(&fs::read_to_string(e.path())?)?;
                    archived += m.src_counts.get(src).copied().unwrap_or(0);
                }
            }
        }
        Ok((hot, archived))
    }

    /// Restore every archive generation of `day` into its hot partition (the
    /// archives stay). Occurrence dedupe makes a second restore a no-op.
    pub fn restore_day(
        &self,
        day: &str,
        min_free_bytes: u64,
    ) -> Result<Vec<ImportOutcome>, StoreError> {
        let archives = self.archives_of(day);
        if archives.is_empty() {
            return Err(StoreError::Other(format!("no archive for {day}")));
        }
        let mut out = Vec::new();
        for (_, path) in archives {
            out.push(self.import_file(
                &path,
                &ImportOptions {
                    from_day: None,
                    to_day: None,
                    min_free_bytes,
                },
            )?);
        }
        Ok(out)
    }

    /// Bytes on disk under the store root (one level of subdirectories:
    /// hot partitions, archives, spool, legacy, sealing, trash).
    pub fn disk_bytes(&self) -> u64 {
        let mut total = 0u64;
        for e in fs::read_dir(&self.root).into_iter().flatten().flatten() {
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                for f in fs::read_dir(e.path()).into_iter().flatten().flatten() {
                    total += f.metadata().map(|m| m.len()).unwrap_or(0);
                }
            } else {
                total += meta.len();
            }
        }
        total
    }

    pub fn status(&self) -> StoreStatus {
        let list = |name: &str| -> Vec<String> {
            let mut v: Vec<String> = fs::read_dir(self.dir(name))
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            v.sort();
            v
        };
        let mut archives: Vec<ArchiveManifest> = fs::read_dir(self.dir("archive"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".manifest.json"))
            .filter_map(|e| {
                fs::read_to_string(e.path())
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
            })
            .collect();
        archives
            .sort_by(|a: &ArchiveManifest, b| (&a.day, a.generation).cmp(&(&b.day, b.generation)));
        let (spooled, lost) = self.counters();
        StoreStatus {
            root: self.root.display().to_string(),
            hot: self
                .hot_days()
                .into_iter()
                .map(|day| PartitionInfo {
                    bytes: fs::metadata(self.hot_path(&day))
                        .map(|m| m.len())
                        .unwrap_or(0),
                    day,
                })
                .collect(),
            archives,
            sealing: list("sealing"),
            trash: list("trash"),
            spool_files: list("spool"),
            legacy_files: list("legacy"),
            pending_legacy: self
                .legacy
                .exists()
                .then(|| self.legacy.display().to_string()),
            imports: self.load_import_state().sources,
            spooled,
            lost,
            free_bytes: free_bytes(&self.root),
        }
    }
}

/// A plain or zstd-compressed file as a byte stream (zstd detected by magic).
fn open_maybe_zstd(path: &Path) -> Result<Box<dyn Read>, StoreError> {
    let mut head = [0u8; 4];
    let n = read_full(&mut File::open(path)?, &mut head)?;
    if n == 4 && head == ZSTD_MAGIC {
        let mut d = zstd::stream::read::Decoder::new(File::open(path)?)?;
        d.window_log_max(31)?;
        Ok(Box::new(d))
    } else {
        Ok(Box::new(File::open(path)?))
    }
}

/// Count `\n`-terminated (and a final unterminated) lines of a zstd stream.
pub fn count_lines_zst(path: &Path) -> Result<u64, StoreError> {
    let mut d = zstd::stream::read::Decoder::new(File::open(path)?)?;
    d.window_log_max(31)?;
    let mut buf = vec![0u8; 4 << 20];
    let mut n = 0u64;
    let mut last = b'\n';
    loop {
        let k = d.read(&mut buf)?;
        if k == 0 {
            break;
        }
        n += buf[..k].iter().filter(|&&b| b == b'\n').count() as u64;
        last = buf[k - 1];
    }
    if last != b'\n' {
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::raw_io::RECORD_VERSION;

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "llmux-rawstore-{}-{}",
            std::process::id(),
            ulid::Ulid::new()
        ));
        fs::create_dir_all(&dir).expect("tmp");
        dir
    }

    fn rec(id: u64, ts_ms: u64, user: &str) -> RawIoRecord {
        RawIoRecord {
            v: RECORD_VERSION,
            ts_ms,
            id,
            group: Some("claude".into()),
            model: Some("claude-opus-5".into()),
            account: Some("acct".into()),
            status: Some(200),
            duration_ms: Some(1200),
            request_body: format!(
                r#"{{"model":"claude-opus-5","metadata":{{"user_id":"{user}"}},"messages":[{{"role":"user","content":"{}"}}]}}"#,
                "hello world ".repeat(50)
            ),
            response_body: r#"{"usage":{"input_tokens":11,"output_tokens":7}}"#.into(),
            request_headers: None,
            response_headers: None,
            upstream: None,
        }
    }

    const DAY_MS: u64 = 86_400_000;
    // 2026-10-27T03:00:00Z
    const T0: u64 = 1_793_070_000_000;

    #[test]
    fn day_keys_follow_the_offset() {
        assert_eq!(day_at_offset(0, 0), "1970-01-01");
        // 2026-10-29T15:30:00Z is 2026-10-30 00:30 in KST (+9h).
        let ts = 1_793_287_800_000;
        assert_eq!(day_at_offset(ts, 0), "2026-10-29");
        assert_eq!(day_at_offset(ts, 9 * 3600), "2026-10-30");
        assert!(is_day("2026-10-30"));
        assert!(!is_day("2026-10-3x"));
    }

    #[test]
    fn live_capture_lands_in_the_day_partition_and_is_found() {
        let dir = tmp();
        let store = RawStore::new(&dir.join("raw-io.jsonl"));
        let r = rec(7, T0, "u1");
        store.capture(&r);
        assert_eq!(store.hot_days(), vec![day_of(T0)]);
        assert_eq!(store.find(7, T0 + 50), Lookup::Found(Box::new(r.clone())));
        assert_eq!(store.find(8, T0), Lookup::NotFound);
        // The stored line is byte-identical to the legacy append format.
        let conn = open_readonly(&store.hot_path(&day_of(T0))).expect("open");
        let line: Vec<u8> = conn
            .query_row("SELECT line FROM raw_io", [], |r| r.get(0))
            .expect("row");
        assert_eq!(line, serde_json::to_vec(&r).expect("json"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn import_is_lossless_and_occurrence_deduplicated() {
        let dir = tmp();
        let legacy = dir.join("raw-io.jsonl");
        let a = serde_json::to_string(&rec(1, T0, "u1")).expect("json");
        let b = serde_json::to_string(&rec(2, T0 + DAY_MS, "u2")).expect("json");
        // Two identical lines (two occurrences), a corrupt line, a blank line
        // and a torn final line.
        let body = format!("{a}\n{a}\nnot json at all\n\n{b}\n{{\"v\":1,\"torn");
        fs::write(&legacy, &body).expect("write");
        let store = RawStore::new(&legacy);
        let out = store
            .import_file(&legacy, &ImportOptions::default())
            .expect("import");
        assert_eq!(out.lines, 5, "a, a, corrupt, b, torn");
        assert_eq!(out.inserted, 5);
        assert_eq!(out.unparsed, 2);
        assert_eq!(out.blank, 1);
        assert_eq!(out.bytes, body.len() as u64);
        assert_eq!(out.sha256, hex(&sha256(body.as_bytes())));
        // Re-import: every occurrence already present.
        let again = store
            .import_file(&legacy, &ImportOptions::default())
            .expect("re-import");
        assert_eq!(again.inserted, 0);
        assert_eq!(again.duplicates, 5);
        // Both days present, plus the fixed partition of the unparsed lines
        // (kept verbatim).
        assert_eq!(store.hot_days().len(), 3);
        assert!(store.hot_days().contains(&UNPARSED_DAY.to_string()));
        let all: Vec<Vec<u8>> = store
            .hot_days()
            .iter()
            .flat_map(|d| {
                let c = open_readonly(&store.hot_path(d)).expect("open");
                let mut s = c
                    .prepare("SELECT line FROM raw_io ORDER BY seq")
                    .expect("prep");
                let v: Vec<Vec<u8>> = s
                    .query_map([], |r| r.get(0))
                    .expect("q")
                    .flatten()
                    .collect();
                v
            })
            .collect();
        assert!(all.contains(&b"not json at all".to_vec()));
        assert!(all.contains(&br#"{"v":1,"torn"#.to_vec()));
        assert_eq!(
            all.iter().filter(|l| l.as_slice() == a.as_bytes()).count(),
            2
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn date_filter_skips_without_marking_imported() {
        let dir = tmp();
        let src = dir.join("backup.jsonl");
        let a = serde_json::to_string(&rec(1, T0, "u1")).expect("json");
        let b = serde_json::to_string(&rec(2, T0 + 2 * DAY_MS, "u2")).expect("json");
        fs::write(&src, format!("{a}\n{b}\n")).expect("write");
        let store = RawStore::new(&dir.join("raw-io.jsonl"));
        let day_b = day_of(T0 + 2 * DAY_MS);
        let out = store
            .import_file(
                &src,
                &ImportOptions {
                    from_day: Some(day_b.clone()),
                    to_day: Some(day_b.clone()),
                    min_free_bytes: 0,
                },
            )
            .expect("import");
        assert_eq!((out.inserted, out.filtered), (1, 1));
        assert_eq!(store.hot_days(), vec![day_b]);
        // A later unfiltered run still imports the skipped day.
        let out = store
            .import_file(&src, &ImportOptions::default())
            .expect("import");
        assert_eq!((out.inserted, out.duplicates), (1, 1));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn archive_seals_past_days_verifies_and_restores() {
        let dir = tmp();
        let store = RawStore::new(&dir.join("raw-io.jsonl"));
        let old = rec(1, T0, "u1");
        let old2 = rec(2, T0 + 1000, "u1");
        let today = rec(3, T0 + 3 * DAY_MS, "u2");
        for r in [&old, &old2, &today] {
            store.capture(r);
        }
        let now = T0 + 3 * DAY_MS + 1000;
        let out = store.archive_cycle(now, 1, false).expect("archive");
        assert_eq!(out.len(), 1, "only the old day is eligible");
        assert_eq!(out[0].rows, 2);
        let old_day = day_of(T0);
        assert!(!store.hot_days().contains(&old_day));
        assert_eq!(store.archives_of(&old_day).len(), 1);
        // The archive is the legacy format: decode → the exact lines.
        assert_eq!(
            count_lines_zst(&store.archives_of(&old_day)[0].1).expect("count"),
            2
        );
        assert_eq!(store.find(1, T0), Lookup::Archived(old_day.clone()));
        // Today still served hot.
        assert_eq!(
            store.find(3, T0 + 3 * DAY_MS),
            Lookup::Found(Box::new(today.clone()))
        );
        // Restore brings the rows back (archive stays), twice is a no-op.
        let r1 = store.restore_day(&old_day, 0).expect("restore");
        assert_eq!(r1[0].inserted, 2);
        let r2 = store.restore_day(&old_day, 0).expect("restore again");
        assert_eq!(r2[0].inserted, 0);
        assert_eq!(store.find(1, T0), Lookup::Found(Box::new(old.clone())));
        assert_eq!(store.archives_of(&old_day).len(), 1);
        // Trash holds the sealed hot DB (not purged before its minimum age).
        assert_eq!(
            store
                .status()
                .trash
                .iter()
                .filter(|n| n.ends_with(".sqlite3"))
                .count(),
            1
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_write_after_sealing_opens_a_new_generation() {
        let dir = tmp();
        let store = RawStore::new(&dir.join("raw-io.jsonl"));
        store.capture(&rec(1, T0, "u1"));
        let day = day_of(T0);
        store.archive_cycle(T0, 0, true).expect("seal today");
        assert!(store.hot_days().is_empty());
        // A straggler for the sealed day: fresh partition, then generation 1.
        store.capture(&rec(2, T0 + 5, "u1"));
        assert_eq!(store.hot_days(), vec![day.clone()]);
        store.archive_cycle(T0, 0, true).expect("seal again");
        let gens: Vec<u32> = store
            .archives_of(&day)
            .into_iter()
            .map(|(g, _)| g)
            .collect();
        assert_eq!(gens, vec![0, 1]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_file_is_imported_reconciled_and_moved_not_deleted() {
        let dir = tmp();
        let legacy = dir.join("raw-io.jsonl");
        let lines: Vec<String> = (0..1100)
            .map(|i| serde_json::to_string(&rec(i, T0 + i * 1000, "u1")).expect("json"))
            .collect();
        let body = lines.join("\n") + "\n";
        fs::write(&legacy, &body).expect("write");
        let store = RawStore::new(&legacy);
        let out = store.import_pending(0, true).expect("import");
        assert_eq!(out[0].inserted, 1100);
        assert!(!legacy.exists(), "moved out of the live path");
        let moved = store.status().legacy_files;
        assert_eq!(moved.len(), 1);
        let kept = fs::read(store.dir("legacy").join(&moved[0])).expect("kept");
        assert_eq!(kept, body.as_bytes(), "legacy bytes kept verbatim");
        let st = store.status();
        let src = st.imports.values().next().expect("state");
        assert!(src.completed);
        assert_eq!(src.lines, 1100);
        assert_eq!(src.offset, body.len() as u64);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unparsed_lines_go_to_a_fixed_partition() {
        let dir = tmp();
        let src = dir.join("x.jsonl");
        fs::write(&src, "garbage one\ngarbage two\n").expect("write");
        let store = RawStore::new(&dir.join("raw-io.jsonl"));
        let out = store
            .import_file(&src, &ImportOptions::default())
            .expect("import");
        assert_eq!((out.inserted, out.unparsed), (2, 2));
        assert_eq!(store.hot_days(), vec![UNPARSED_DAY.to_string()]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reconciliation_mismatch_restarts_the_source_and_recovers() {
        let dir = tmp();
        let legacy = dir.join("raw-io.jsonl");
        let lines: Vec<String> = (0..10)
            .map(|i| serde_json::to_string(&rec(i, T0 + i * 1000, "u1")).expect("json"))
            .collect();
        let body = lines.join("\n") + "\n";
        fs::write(&legacy, &body).expect("write");
        let store = RawStore::new(&legacy);
        // A checkpoint that claims everything was imported, with no rows
        // behind it (rows lost after a durable checkpoint).
        let identity = source_identity_of_file(&legacy).expect("id");
        let mut state = ImportState::default();
        state.sources.insert(
            identity,
            SourceState {
                path: legacy.display().to_string(),
                offset: body.len() as u64,
                lines: 10,
                ..SourceState::default()
            },
        );
        store.save_import_state(&state).expect("state");
        assert!(
            store.import_pending(0, true).is_err(),
            "mismatch is reported, not moved"
        );
        assert!(legacy.exists(), "source kept in place");
        // Next cycle re-imports from 0 and reconciles.
        let out = store.import_pending(0, true).expect("recovered");
        assert_eq!(out[0].inserted, 10);
        assert!(!legacy.exists());
        assert_eq!(store.status().legacy_files.len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reconciliation_never_resets_a_partly_archived_source() {
        let dir = tmp();
        let legacy = dir.join("raw-io.jsonl");
        let lines: Vec<String> = (0..4)
            .map(|i| serde_json::to_string(&rec(i, T0 + i * DAY_MS, "u1")).expect("json"))
            .collect();
        let body = lines.join("\n") + "\n";
        fs::write(&legacy, &body).expect("write");
        let store = RawStore::new(&legacy);
        // Import (same identity as the auto-import), then seal the old days.
        store
            .import_file(&legacy, &ImportOptions::default())
            .expect("import");
        store
            .archive_cycle(T0 + 3 * DAY_MS, 0, false)
            .expect("archive");
        let identity = source_identity_of_file(&legacy).expect("id");
        let (hot, archived) = store.count_rows_of_source(&identity).expect("count");
        assert_eq!(hot + archived, 4);
        assert!(archived > 0);
        // A state that claims one more line than exist: mismatch with
        // archived rows → refuse the reset, keep the file, keep the state.
        let mut state = ImportState::default();
        state.sources.insert(
            identity.clone(),
            SourceState {
                path: legacy.display().to_string(),
                offset: body.len() as u64,
                lines: 5,
                ..SourceState::default()
            },
        );
        store.save_import_state(&state).expect("state");
        assert!(store.import_pending(0, true).is_err());
        assert!(legacy.exists(), "source kept");
        let kept = store.load_import_state();
        assert_eq!(
            kept.sources[&identity].offset,
            body.len() as u64,
            "no reset to 0"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_day_open_for_writing_elsewhere_is_not_sealed() {
        let dir = tmp();
        let legacy = dir.join("raw-io.jsonl");
        // Two store instances on one root stand in for two daemons (the
        // lock is a file lock, so the second instance contends like a
        // second process would).
        let draining = RawStore::new(&legacy);
        let successor = RawStore::new(&legacy);
        draining.capture(&rec(1, T0, "u1"));
        let day = day_of(T0);
        let out = successor.archive_cycle(T0, 0, true).expect("cycle");
        assert!(out.is_empty(), "the other writer holds the day");
        assert_eq!(successor.hot_days(), vec![day.clone()]);
        // Once the other writer is gone the day seals.
        drop(draining);
        let out = successor.archive_cycle(T0, 0, true).expect("cycle");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rows, 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn spool_generations_are_imported() {
        let dir = tmp();
        let store = RawStore::new(&dir.join("raw-io.jsonl"));
        let r = rec(9, T0, "u1");
        store.spool(serde_json::to_string(&r).expect("json").as_bytes());
        assert_eq!(store.counters(), (1, 0));
        store.import_pending(0, true).expect("import");
        assert_eq!(store.find(9, T0), Lookup::Found(Box::new(r)));
        assert!(!store.dir("spool").join("current.jsonl").exists());
        assert_eq!(store.status().legacy_files.len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn session_rows_carry_metadata_without_bodies() {
        let dir = tmp();
        let store = RawStore::new(&dir.join("raw-io.jsonl"));
        store.capture(&rec(1, T0, "u1"));
        let mut rows = Vec::new();
        store.for_each_session_rows(false, |r| {
            rows.extend(r);
            true
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].user_id.as_deref(), Some("u1"));
        assert_eq!((rows[0].tokens_in, rows[0].tokens_out), (11, 7));
        let _ = fs::remove_dir_all(dir);
    }
}
