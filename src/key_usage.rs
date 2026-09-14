//! Durable per-tenant usage store (`docs/keys-history/spec.md`, trace K).
//!
//! The keys panel used to read LIFETIME aggregates the activity fold keeps in
//! RAM: no windows, no filters, and everything older than the in-memory ring
//! only as long as `activity.jsonl` could be replayed at boot. This module is
//! the durable half: ONE indexed SQLite table of finished-request metadata,
//! queried with an exact rolling window + model filter and nothing else.
//!
//! Deliberate boundaries:
//!
//! - **Metadata only.** A row is `(when, tenant, group, model, status, token
//!   counts)`. No prompt, no excerpt, no credential, no digest — the panel
//!   never needs them and the file sits next to the config that does hold
//!   secrets, so it is created owner-only (0600 in a 0700 directory).
//! - **Keys-only.** `activity.jsonl` keeps serving every other feature
//!   (activity ring, sessions, per-model stats, infinite scroll). This store
//!   is additive; nothing is deleted or migrated away from it.
//! - **Never on the serving path.** Writes go to a bounded queue drained by
//!   one dedicated thread ([`KeyUsageStore::record`] does no disk IO and never
//!   blocks), reads are bounded indexed `GROUP BY` queries meant for
//!   `spawn_blocking` — never a per-frame query, never a full-history clone.
//! - **Coverage is what the fold sees.** Rows come from
//!   `ActivityEvent::RequestFinished`, which the proxy `try_send`s
//!   best-effort: an event dropped on a full activity channel is not billed
//!   here either. This store is faithful metering of *observed completions*,
//!   not an independent request ledger — and a failed write is surfaced
//!   ([`UsageHealth`]), never rendered as a valid zero.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::dashboard::{KeyRowDoc, TenantModelDoc, TenantUsageDoc};
use crate::tui::activity::normalize_model;
use crate::tui::{ActivityEvent, TokenCounts};

/// Schema version stored in `meta`. Bump only with a migration.
pub const SCHEMA_VERSION: u32 = 1;

/// Schema version of `activity.jsonl` lines this importer understands — the
/// SAME constant the writer stamps them with ([`crate::tui::activity`]), not a
/// copy that can drift. Lines carrying any other version are skipped, exactly
/// like the activity replay skips them.
use crate::tui::activity::PERSIST_VERSION as LEGACY_PERSIST_VERSION;

/// Rows buffered for the writer thread before new ones are dropped. Metering
/// must never apply backpressure to the fold (same rule as the activity
/// channel); a drop is counted and surfaced in [`UsageHealth::dropped`].
const WRITE_QUEUE_MAX: u64 = 20_000;

/// Rows per import transaction: bounds the write-lock hold time and makes a
/// crash mid-import resume from the last committed offset.
const IMPORT_CHUNK: usize = 2_000;

/// The bucket an unattributed request is booked against — pre-tenant legacy
/// history. Deliberately NOT coerced into a live bucket. Mirrors
/// [`crate::tui::activity`]'s `unknown` client bucket.
const UNKNOWN_TENANT: &str = "unknown";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum UsageError {
    #[error("usage store at {path}: {source}")]
    Sqlite {
        path: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("usage store io at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl UsageError {
    fn sqlite(path: &str, source: rusqlite::Error) -> Self {
        Self::Sqlite {
            path: path.to_string(),
            source,
        }
    }

    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

/// Trailing window the keys panel aggregates over. Every non-`All` variant is
/// an EXACT closed interval `[now - duration, now]`; `All` is `[0, now]`. No
/// variant ever includes a future stamp — a clock-skewed row must not invent
/// usage that has not happened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageWindow {
    /// Full retained history — the backwards-compatible default.
    #[default]
    All,
    H1,
    H24,
    D7,
    D14,
    D30,
    D90,
}

impl UsageWindow {
    /// Every offered window, in the order `w` cycles them.
    pub const ALL: [UsageWindow; 7] = [
        Self::All,
        Self::H1,
        Self::H24,
        Self::D7,
        Self::D14,
        Self::D30,
        Self::D90,
    ];

    /// Wire/label form (`all`, `1h`, `24h`, `7d`, `14d`, `30d`, `90d`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::H1 => "1h",
            Self::H24 => "24h",
            Self::D7 => "7d",
            Self::D14 => "14d",
            Self::D30 => "30d",
            Self::D90 => "90d",
        }
    }

    /// Parse the wire form. `None` for anything else — an unknown window is a
    /// client error, never a silent fallback to `all`.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.as_str() == raw)
    }

    /// Window length in millis; `None` for [`Self::All`].
    pub fn duration_ms(&self) -> Option<u64> {
        let hour = 3_600_000_u64;
        Some(match self {
            Self::All => return None,
            Self::H1 => hour,
            Self::H24 => 24 * hour,
            Self::D7 => 7 * 24 * hour,
            Self::D14 => 14 * 24 * hour,
            Self::D30 => 30 * 24 * hour,
            Self::D90 => 90 * 24 * hour,
        })
    }

    /// Next window in the `w` cycle (wraps).
    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|w| *w == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    /// Inclusive lower bound of the window at `now_ms` (0 for `All`).
    pub fn from_ms(&self, now_ms: u64) -> u64 {
        match self.duration_ms() {
            Some(d) => now_ms.saturating_sub(d),
            None => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Query + report
// ---------------------------------------------------------------------------

/// One bounded keys-usage query: a window, an optional exact-name model
/// filter, and the instant the window is anchored at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageQuery {
    pub window: UsageWindow,
    /// Selected models (already normalized). EMPTY = every model, and then —
    /// and only then — requests with no attributed model are included too.
    pub models: Vec<String>,
    pub now_ms: u64,
}

impl UsageQuery {
    pub fn new(window: UsageWindow, now_ms: u64) -> Self {
        Self {
            window,
            models: Vec::new(),
            now_ms,
        }
    }

    /// Restrict to these models (normalized + deduped + sorted, so the query
    /// and the echoed filter are canonical).
    pub fn with_models<I, S>(mut self, models: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let set: BTreeSet<String> = models
            .into_iter()
            .map(|m| normalize_model(m.as_ref()))
            .filter(|m| !m.is_empty())
            .collect();
        self.models = set.into_iter().collect();
        self
    }
}

/// One tenant's aggregate over the queried window (unpriced — pricing needs
/// the daemon's overrides and happens one layer up, in [`usage_doc`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TenantUsage {
    pub tenant: String,
    pub requests: u64,
    pub ok: u64,
    pub errors: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// First/last matched request in the window (epoch ms; 0 = none).
    pub first_ms: u64,
    pub last_ms: u64,
    /// Per-(group, model) cells, sorted by total tokens desc.
    pub models: Vec<ModelUsage>,
}

/// One tenant's usage of one served model within the window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelUsage {
    pub group: String,
    pub model: String,
    pub requests: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

/// The result of one [`UsageQuery`]: the matched tenant rows plus the filter
/// metadata the UI echoes (so a filtered view can never be relabelled as
/// lifetime data).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageReport {
    pub window: UsageWindow,
    /// The applied model filter (empty = all).
    pub models: Vec<String>,
    /// Distinct models OBSERVED in the window — what the picker offers.
    pub available_models: Vec<String>,
    pub from_ms: u64,
    pub to_ms: u64,
    /// Matched request count (the receipt that "empty" means empty).
    pub rows: u64,
    pub tenants: Vec<TenantUsage>,
}

// ---------------------------------------------------------------------------
// Row
// ---------------------------------------------------------------------------

/// One durable usage row: finished-request metadata, nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageRow {
    pub ts_ms: u64,
    /// Per-process request id — with `ts_ms` it forms the dedup identity that
    /// makes the legacy import idempotent against live writes.
    pub request_id: u64,
    pub tenant: String,
    pub group: Option<String>,
    /// Normalized served model ([`normalize_model`]); `None` when the request
    /// failed before routing.
    pub model: Option<String>,
    pub status: u16,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl UsageRow {
    pub fn new(
        ts_ms: u64,
        request_id: u64,
        tenant: Option<&str>,
        group: Option<&str>,
        model: Option<&str>,
        status: u16,
    ) -> Self {
        Self {
            ts_ms,
            request_id,
            tenant: match tenant {
                Some(id) if !id.is_empty() => id.to_string(),
                _ => UNKNOWN_TENANT.to_string(),
            },
            group: group.map(str::to_string),
            model: model
                .map(normalize_model)
                .filter(|m: &String| !m.is_empty()),
            status,
            ..Default::default()
        }
    }

    pub fn with_tokens(
        mut self,
        tokens_in: u64,
        tokens_out: u64,
        cache_read: u64,
        cache_creation: u64,
    ) -> Self {
        self.tokens_in = tokens_in;
        self.tokens_out = tokens_out;
        self.cache_read = cache_read;
        self.cache_creation = cache_creation;
        self
    }

    /// Build a row from a finished-request event folded at `now`. `None` for
    /// every other event variant (only completions are metered).
    pub fn from_event(event: &ActivityEvent, now: SystemTime) -> Option<Self> {
        let ActivityEvent::RequestFinished {
            id,
            status,
            tokens,
            group,
            model,
            tenant,
            ..
        } = event
        else {
            return None;
        };
        let ts_ms = epoch_ms(now);
        let row = Self::new(
            ts_ms,
            *id,
            tenant.as_deref(),
            group.as_deref(),
            model.as_deref(),
            *status,
        );
        Some(match tokens {
            Some(t) => row.with_tokens(
                t.input,
                t.output,
                t.cache_read.unwrap_or(0),
                t.cache_creation.unwrap_or(0),
            ),
            None => row,
        })
    }

    /// Stable identity: the same finished request produces the same key
    /// whether it arrives live or through the legacy import, which is what
    /// makes an overlapping import a no-op instead of a double count.
    pub fn event_key(&self) -> String {
        format!("{}:{}", self.ts_ms, self.request_id)
    }
}

fn epoch_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Legacy import
// ---------------------------------------------------------------------------

/// What one [`UsageDb::import_activity_jsonl`] pass did. `imported +
/// duplicates + skipped` is every complete line it read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportOutcome {
    /// Rows newly inserted.
    pub imported: u64,
    /// Rows already present under the same identity (live/overlap safety).
    pub duplicates: u64,
    /// Lines skipped: unparseable, or an unknown schema version.
    pub skipped: u64,
    /// Bytes the persisted offset advanced by.
    pub bytes: u64,
}

/// The subset of a persisted `activity.jsonl` record this store meters. A
/// tolerant reader by design: unknown fields are ignored and absent optional
/// fields carry their pre-field meaning (`tenant: None` → `unknown`).
#[derive(Debug, Deserialize)]
struct LegacyLine {
    v: u8,
    ts_ms: u64,
    id: u64,
    status: u16,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    tenant: Option<String>,
    #[serde(default)]
    tokens: Option<TokenCounts>,
}

// ---------------------------------------------------------------------------
// The database
// ---------------------------------------------------------------------------

/// The SQLite store itself: one connection, synchronous API. Wrap it in
/// [`KeyUsageStore`] for the daemon (async-safe writes); use it directly in
/// tests and one-shot tooling.
pub struct UsageDb {
    conn: Connection,
    path: String,
}

impl UsageDb {
    /// Open (creating if needed) the store at `path`. The parent directory is
    /// created 0700 and the file is chmod'ed 0600 — it holds per-tenant
    /// metering next to the credential-bearing config.
    pub fn open(path: &Path) -> Result<Self, UsageError> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() && !dir.exists() {
                std::fs::create_dir_all(dir).map_err(|e| UsageError::io(dir, e))?;
                set_mode(dir, 0o700);
            }
        }
        let display = path.display().to_string();
        let conn = Connection::open(path).map_err(|e| UsageError::sqlite(&display, e))?;
        set_mode(path, 0o600);
        Self::init(conn, display)
    }

    /// An ephemeral store — tests and any caller with no durable location.
    pub fn open_in_memory() -> Result<Self, UsageError> {
        let conn = Connection::open_in_memory().map_err(|e| UsageError::sqlite(":memory:", e))?;
        Self::init(conn, ":memory:".to_string())
    }

    fn init(conn: Connection, path: String) -> Result<Self, UsageError> {
        conn.execute_batch(
            "PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS usage (
                 event_key      TEXT PRIMARY KEY,
                 ts_ms          INTEGER NOT NULL,
                 tenant         TEXT NOT NULL,
                 grp            TEXT,
                 model          TEXT,
                 status         INTEGER NOT NULL,
                 tokens_in      INTEGER NOT NULL DEFAULT 0,
                 tokens_out     INTEGER NOT NULL DEFAULT 0,
                 cache_read     INTEGER NOT NULL DEFAULT 0,
                 cache_creation INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS usage_ts_idx ON usage(ts_ms);
             CREATE INDEX IF NOT EXISTS usage_model_ts_idx ON usage(model, ts_ms);
             CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);",
        )
        .map_err(|e| UsageError::sqlite(&path, e))?;
        let db = Self { conn, path };
        db.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        Ok(db)
    }

    /// Where this store lives (`:memory:` for the ephemeral one).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Record one finished request. `Ok(false)` = an identical identity was
    /// already stored (the import/live overlap case), inserted nothing.
    pub fn insert(&self, row: &UsageRow) -> Result<bool, UsageError> {
        let changed = self
            .conn
            .execute(INSERT_SQL, rusqlite::params_from_iter(insert_values(row)))
            .map_err(|e| UsageError::sqlite(&self.path, e))?;
        Ok(changed > 0)
    }

    /// Record a batch in one transaction (the writer thread's drain).
    pub fn insert_many(&mut self, rows: &[UsageRow]) -> Result<u64, UsageError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| UsageError::sqlite(&self.path, e))?;
        let mut inserted = 0;
        {
            let mut stmt = tx
                .prepare_cached(INSERT_SQL)
                .map_err(|e| UsageError::sqlite(&self.path, e))?;
            for row in rows {
                inserted +=
                    stmt.execute(rusqlite::params_from_iter(insert_values(row)))
                        .map_err(|e| UsageError::sqlite(&self.path, e))? as u64;
            }
        }
        tx.commit().map_err(|e| UsageError::sqlite(&self.path, e))?;
        Ok(inserted)
    }

    /// Total stored rows (a receipt for migration/restart equality).
    pub fn row_count(&self) -> Result<u64, UsageError> {
        self.conn
            .query_row("SELECT COUNT(*) FROM usage", [], |r| r.get::<_, i64>(0))
            .map(|n| n as u64)
            .map_err(|e| UsageError::sqlite(&self.path, e))
    }

    /// Run one bounded, indexed aggregate query. The SQL returns at most one
    /// row per `(tenant, group, model)` present in the window — never the
    /// underlying request rows, so cost is bounded by the ANSWER, not by how
    /// much history exists.
    pub fn query(&self, query: &UsageQuery) -> Result<UsageReport, UsageError> {
        let from_ms = query.window.from_ms(query.now_ms);
        let to_ms = query.now_ms;
        let mut sql = String::from(
            "SELECT tenant, grp, model, COUNT(*), \
             SUM(CASE WHEN status < 400 THEN 1 ELSE 0 END), \
             SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END), \
             SUM(tokens_in), SUM(tokens_out), SUM(cache_read), SUM(cache_creation), \
             MIN(ts_ms), MAX(ts_ms) \
             FROM usage WHERE ts_ms >= ?1 AND ts_ms <= ?2",
        );
        let mut params: Vec<rusqlite::types::Value> = vec![
            rusqlite::types::Value::Integer(from_ms as i64),
            rusqlite::types::Value::Integer(to_ms as i64),
        ];
        if !query.models.is_empty() {
            sql.push_str(" AND model IN (");
            for (i, model) in query.models.iter().enumerate() {
                if i > 0 {
                    sql.push(',');
                }
                sql.push_str(&format!("?{}", params.len() + 1));
                params.push(rusqlite::types::Value::Text(model.clone()));
            }
            sql.push(')');
        }
        sql.push_str(" GROUP BY tenant, grp, model");

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| UsageError::sqlite(&self.path, e))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params), |r| {
                Ok(GroupRow {
                    tenant: r.get(0)?,
                    group: r.get(1)?,
                    model: r.get(2)?,
                    requests: r.get::<_, i64>(3)? as u64,
                    ok: r.get::<_, i64>(4)? as u64,
                    errors: r.get::<_, i64>(5)? as u64,
                    tokens_in: r.get::<_, i64>(6)? as u64,
                    tokens_out: r.get::<_, i64>(7)? as u64,
                    cache_read: r.get::<_, i64>(8)? as u64,
                    cache_creation: r.get::<_, i64>(9)? as u64,
                    first_ms: r.get::<_, i64>(10)? as u64,
                    last_ms: r.get::<_, i64>(11)? as u64,
                })
            })
            .map_err(|e| UsageError::sqlite(&self.path, e))?;

        let mut tenants: BTreeMap<String, TenantUsage> = BTreeMap::new();
        let mut total = 0_u64;
        for row in rows {
            let row = row.map_err(|e| UsageError::sqlite(&self.path, e))?;
            total += row.requests;
            let entry = tenants
                .entry(row.tenant.clone())
                .or_insert_with(|| TenantUsage {
                    tenant: row.tenant.clone(),
                    ..Default::default()
                });
            entry.requests += row.requests;
            entry.ok += row.ok;
            entry.errors += row.errors;
            entry.tokens_in = entry.tokens_in.saturating_add(row.tokens_in);
            entry.tokens_out = entry.tokens_out.saturating_add(row.tokens_out);
            if entry.first_ms == 0 || row.first_ms < entry.first_ms {
                entry.first_ms = row.first_ms;
            }
            entry.last_ms = entry.last_ms.max(row.last_ms);
            // Only model-attributed requests get a breakdown cell; pre-routing
            // failures stay in the tenant totals (same rule as the in-memory
            // fold), so the two can legitimately disagree on token sums.
            if let (Some(group), Some(model)) = (row.group.clone(), row.model.clone()) {
                entry.models.push(ModelUsage {
                    group,
                    model,
                    requests: row.requests,
                    tokens_in: row.tokens_in,
                    tokens_out: row.tokens_out,
                    cache_read: row.cache_read,
                    cache_creation: row.cache_creation,
                });
            }
        }

        let mut tenants: Vec<TenantUsage> = tenants.into_values().collect();
        for tenant in &mut tenants {
            tenant.models.sort_by(|a, b| {
                total_tokens(b)
                    .cmp(&total_tokens(a))
                    .then(a.model.cmp(&b.model))
            });
        }
        tenants.sort_by(|a, b| {
            b.requests
                .cmp(&a.requests)
                .then((b.tokens_in + b.tokens_out).cmp(&(a.tokens_in + a.tokens_out)))
                .then(a.tenant.cmp(&b.tenant))
        });

        Ok(UsageReport {
            window: query.window,
            models: query.models.clone(),
            // Offered models come from a WINDOW-only distinct query: derived
            // from the filtered aggregate, picking A would have hidden B and C,
            // so a filter could only ever be narrowed and never changed.
            available_models: self.observed_models(from_ms, to_ms)?,
            from_ms,
            to_ms,
            rows: total,
            tenants,
        })
    }

    /// Distinct served models observed in `[from_ms, to_ms]`, sorted. The
    /// picker's option list: window-scoped (so it reflects the period on
    /// screen) but NEVER model-filtered. Indexed by `usage_model_ts_idx`.
    fn observed_models(&self, from_ms: u64, to_ms: u64) -> Result<Vec<String>, UsageError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DISTINCT model FROM usage \
                 WHERE model IS NOT NULL AND ts_ms >= ?1 AND ts_ms <= ?2 \
                 ORDER BY model",
            )
            .map_err(|e| UsageError::sqlite(&self.path, e))?;
        let rows = stmt
            .query_map([from_ms as i64, to_ms as i64], |r| r.get::<_, String>(0))
            .map_err(|e| UsageError::sqlite(&self.path, e))?;
        rows.collect::<Result<Vec<String>, _>>()
            .map_err(|e| UsageError::sqlite(&self.path, e))
    }

    // -- legacy import ------------------------------------------------------

    /// Identity of the source file the stored offset belongs to (empty when
    /// never imported).
    fn import_witness(&self, path: &Path) -> Result<String, UsageError> {
        Ok(self.get_meta(&witness_key(path))?.unwrap_or_default())
    }

    /// Start this source over: offset 0 + the new witness, atomically.
    fn reset_import(&mut self, path: &Path, witness: &str) -> Result<(), UsageError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| UsageError::sqlite(&self.path, e))?;
        for (k, v) in [
            (offset_key(path), "0".to_string()),
            (witness_key(path), witness.to_string()),
        ] {
            tx.execute(
                "INSERT INTO meta (k, v) VALUES (?1, ?2) \
                 ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                rusqlite::params![k, v],
            )
            .map_err(|e| UsageError::sqlite(&self.path, e))?;
        }
        tx.commit().map_err(|e| UsageError::sqlite(&self.path, e))
    }

    /// Byte offset of `path` already imported (0 when never).
    pub fn import_offset(&self, path: &Path) -> Result<u64, UsageError> {
        Ok(self
            .get_meta(&offset_key(path))?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0))
    }

    /// Import finished requests from a legacy `activity.jsonl` prefix, from
    /// the persisted offset up to `up_to` bytes (the cut the daemon recorded
    /// BEFORE it started appending live).
    ///
    /// Idempotent twice over: the offset is committed in the same transaction
    /// as the rows it covers (so a re-run imports nothing), and every row
    /// carries the same identity it would have been written live under (so an
    /// overlapping range cannot double-count). Only COMPLETE lines advance the
    /// offset — a half-written trailing record is left for the next pass. A
    /// source that shrank (rotation) restarts from zero.
    ///
    /// Blocking file IO — call it from `spawn_blocking`.
    pub fn import_activity_jsonl(
        &mut self,
        path: &Path,
        up_to: u64,
    ) -> Result<ImportOutcome, UsageError> {
        let mut outcome = ImportOutcome::default();
        // One chunk per pass, exactly like the daemon's chunked driver — the
        // idempotence lives in `import_range`/`commit_import`, which both
        // callers share, so there is only ONE definition of "already imported".
        while let Some((offset, end)) = self.import_range(path, up_to)? {
            let chunk = read_chunk(path, offset, end, IMPORT_CHUNK)?;
            if chunk.bytes == 0 {
                break; // torn trailing record: leave it for the next pass
            }
            let inserted = self.commit_import(path, &chunk.rows, offset + chunk.bytes)?;
            outcome.imported += inserted;
            outcome.duplicates += chunk.rows.len() as u64 - inserted;
            outcome.skipped += chunk.skipped;
            outcome.bytes += chunk.bytes;
        }
        Ok(outcome)
    }

    /// Resolve what is left to import: `Some((offset, end))`, or `None` when
    /// the source is absent or already consumed. Cheap (one stat + two meta
    /// reads), so the daemon can call it once per chunk and keep the store
    /// lock held only for this and the commit — never across the parse.
    ///
    /// Re-checks rotation every call: a source replaced MID-import is caught
    /// on the next chunk instead of writing the new file's bytes at the old
    /// file's offset.
    pub(crate) fn import_range(
        &mut self,
        path: &Path,
        up_to: u64,
    ) -> Result<Option<(u64, u64)>, UsageError> {
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            // ONLY "not there yet" is benign (first boot). A permission or
            // path error must surface — an unreadable history must never be
            // indistinguishable from an empty one.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(UsageError::io(path, err)),
        };
        let len = meta.len();
        let mut offset = self.import_offset(path)?;
        // Three rotation signals, because none alone is enough: a shrunken
        // file (copytruncate) is caught by the offset, a REPLACED file of
        // equal/greater length only by its file id, and an IN-PLACE refill to
        // the same length only by its head bytes.
        let witness = source_witness(path, &meta)?;
        if offset > len || self.import_witness(path)? != witness {
            // Reset the offset AND record the new witness in ONE transaction.
            // Written separately, a crash in between would leave "new source,
            // old offset" — and the head of that file would be skipped forever.
            self.reset_import(path, &witness)?;
            offset = 0;
        }
        let end = up_to.min(len);
        if end <= offset {
            return Ok(None);
        }
        Ok(Some((offset, end)))
    }

    /// Commit one import chunk: its rows AND the new offset in ONE
    /// transaction, so a crash can neither lose nor replay them.
    pub(crate) fn commit_import(
        &mut self,
        source: &Path,
        rows: &[UsageRow],
        new_offset: u64,
    ) -> Result<u64, UsageError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| UsageError::sqlite(&self.path, e))?;
        let mut inserted = 0_u64;
        {
            let mut stmt = tx
                .prepare_cached(INSERT_SQL)
                .map_err(|e| UsageError::sqlite(&self.path, e))?;
            for row in rows {
                inserted +=
                    stmt.execute(rusqlite::params_from_iter(insert_values(row)))
                        .map_err(|e| UsageError::sqlite(&self.path, e))? as u64;
            }
        }
        tx.execute(
            "INSERT INTO meta (k, v) VALUES (?1, ?2) \
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            rusqlite::params![offset_key(source), new_offset.to_string()],
        )
        .map_err(|e| UsageError::sqlite(&self.path, e))?;
        tx.commit().map_err(|e| UsageError::sqlite(&self.path, e))?;
        Ok(inserted)
    }

    fn get_meta(&self, key: &str) -> Result<Option<String>, UsageError> {
        self.conn
            .query_row("SELECT v FROM meta WHERE k = ?1", [key], |r| {
                r.get::<_, String>(0)
            })
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(UsageError::sqlite(&self.path, other)),
            })
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<(), UsageError> {
        self.conn
            .execute(
                "INSERT INTO meta (k, v) VALUES (?1, ?2) \
                 ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                rusqlite::params![key, value],
            )
            .map(|_| ())
            .map_err(|e| UsageError::sqlite(&self.path, e))
    }
}

const INSERT_SQL: &str = "INSERT OR IGNORE INTO usage \
     (event_key, ts_ms, tenant, grp, model, status, tokens_in, tokens_out, cache_read, cache_creation) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

/// Owned bind values for [`INSERT_SQL`] (owned, so the array outlives the
/// statement call without borrowing a temporary).
fn insert_values(row: &UsageRow) -> [rusqlite::types::Value; 10] {
    use rusqlite::types::Value;
    let text = |v: &Option<String>| match v {
        Some(v) => Value::Text(v.clone()),
        None => Value::Null,
    };
    [
        Value::Text(row.event_key()),
        Value::Integer(row.ts_ms as i64),
        Value::Text(row.tenant.clone()),
        text(&row.group),
        text(&row.model),
        Value::Integer(row.status as i64),
        Value::Integer(row.tokens_in as i64),
        Value::Integer(row.tokens_out as i64),
        Value::Integer(row.cache_read as i64),
        Value::Integer(row.cache_creation as i64),
    ]
}

struct GroupRow {
    tenant: String,
    group: Option<String>,
    model: Option<String>,
    requests: u64,
    ok: u64,
    errors: u64,
    tokens_in: u64,
    tokens_out: u64,
    cache_read: u64,
    cache_creation: u64,
    first_ms: u64,
    last_ms: u64,
}

fn total_tokens(m: &ModelUsage) -> u64 {
    m.tokens_in
        .saturating_add(m.tokens_out)
        .saturating_add(m.cache_read)
        .saturating_add(m.cache_creation)
}

/// One parsed slice of a legacy log: the rows it yielded, how many lines were
/// unusable, and how many BYTES of complete lines it consumed (what the
/// persisted offset advances by).
pub(crate) struct ImportChunk {
    pub rows: Vec<UsageRow>,
    pub skipped: u64,
    pub bytes: u64,
}

/// Read and parse at most `max_rows` complete lines of `path` starting at
/// `offset`, never past `end`. Streaming (one line at a time, bounded buffer)
/// — a half-gigabyte activity log is never copied into memory — and holds NO
/// lock, which is what lets an import interleave with live writes and queries.
pub(crate) fn read_chunk(
    path: &Path,
    offset: u64,
    end: u64,
    max_rows: usize,
) -> Result<ImportChunk, UsageError> {
    let mut chunk = ImportChunk {
        rows: Vec::new(),
        skipped: 0,
        bytes: 0,
    };
    if end <= offset {
        return Ok(chunk);
    }
    let file = std::fs::File::open(path).map_err(|e| UsageError::io(path, e))?;
    let mut reader = BufReader::new(file);
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|e| UsageError::io(path, e))?;
    let mut reader = reader.take(end - offset);
    let mut line = String::new();
    while chunk.rows.len() < max_rows {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|e| UsageError::io(path, e))?;
        if read == 0 || !line.ends_with('\n') {
            break; // end of range, or a torn trailing record
        }
        chunk.bytes += read as u64;
        match parse_legacy_line(&line) {
            Some(row) => chunk.rows.push(row),
            None => chunk.skipped += 1,
        }
    }
    Ok(chunk)
}

fn offset_key(path: &Path) -> String {
    format!("import_offset:{}", path.display())
}

fn witness_key(path: &Path) -> String {
    format!("import_source:{}", path.display())
}

/// Bytes of the source head that identify its content. Appending never
/// touches them, so this is stable for the normal case and differs the moment
/// the file is rewritten.
const WITNESS_HEAD_BYTES: usize = 4096;

/// Identity of the imported file: its file id (changes when the log is
/// REPLACED, even by a same-size file) plus a digest of its head (changes when
/// the log is truncated and refilled IN PLACE, which keeps the same file id).
/// Either difference means the stored byte offset points into a file that no
/// longer exists, so the import restarts — row identity keeps the counts exact.
fn source_witness(path: &Path, meta: &std::fs::Metadata) -> Result<String, UsageError> {
    #[cfg(unix)]
    let id = {
        use std::os::unix::fs::MetadataExt as _;
        format!("{}:{}", meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let id = String::from("-");
    let mut head = vec![0_u8; WITNESS_HEAD_BYTES];
    let mut file = std::fs::File::open(path).map_err(|e| UsageError::io(path, e))?;
    let read = read_head(&mut file, &mut head).map_err(|e| UsageError::io(path, e))?;
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&head[..read]);
    let mut digest = String::with_capacity(64);
    for b in hasher.finalize() {
        use std::fmt::Write as _;
        let _ = write!(digest, "{b:02x}");
    }
    Ok(format!("{id}:{digest}"))
}

/// Fill `buf` from `file` until it is full or the file ends (a short `read` is
/// not an EOF). Returns how many bytes landed.
fn read_head(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

fn parse_legacy_line(line: &str) -> Option<UsageRow> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed: LegacyLine = serde_json::from_str(trimmed).ok()?;
    if parsed.v != LEGACY_PERSIST_VERSION {
        return None;
    }
    let row = UsageRow::new(
        parsed.ts_ms,
        parsed.id,
        parsed.tenant.as_deref(),
        parsed.group.as_deref(),
        parsed.model.as_deref(),
        parsed.status,
    );
    Some(match parsed.tokens {
        Some(t) => row.with_tokens(
            t.input,
            t.output,
            t.cache_read.unwrap_or(0),
            t.cache_creation.unwrap_or(0),
        ),
        None => row,
    })
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

// ---------------------------------------------------------------------------
// Async-safe store
// ---------------------------------------------------------------------------

/// Live health of the store — the receipt that makes a failure visible
/// instead of letting a broken write render as a valid zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageHealth {
    /// Rows durably written since start.
    pub written: u64,
    /// Rows dropped because the write queue was full (never backpressure).
    pub dropped: u64,
    /// Failed write batches.
    pub errors: u64,
    /// A legacy-history import is RUNNING: the aggregates answered right now
    /// are partial and must be labelled as such, never read as final totals.
    #[serde(default)]
    pub importing: bool,
    /// Percent of the legacy source consumed by that import (100 at rest).
    #[serde(default)]
    pub import_pct: u8,
    /// Most recent write error, if any.
    #[serde(default)]
    pub last_error: Option<String>,
    /// Where the store lives.
    #[serde(default)]
    pub path: Option<String>,
}

struct Counters {
    written: AtomicU64,
    dropped: AtomicU64,
    errors: AtomicU64,
    queued: AtomicU64,
    /// A legacy import is in progress (see [`UsageHealth::importing`]).
    importing: std::sync::atomic::AtomicBool,
    /// Percent of that import's source consumed; 100 at rest.
    import_pct: std::sync::atomic::AtomicU8,
    last_error: Mutex<Option<String>>,
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            written: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            queued: AtomicU64::new(0),
            importing: std::sync::atomic::AtomicBool::new(false),
            // Nothing to import until one starts: a fresh store is 100% caught
            // up, not 0% loaded.
            import_pct: std::sync::atomic::AtomicU8::new(100),
            last_error: Mutex::new(None),
        }
    }
}

/// The daemon-facing store: a [`UsageDb`] plus one dedicated writer thread.
///
/// [`Self::record`] is a queue push — no disk IO, no blocking, safe to call
/// from the event fold while it holds the hub lock. [`Self::query`] and
/// [`Self::import_activity_jsonl`] block and belong on `spawn_blocking`.
pub struct KeyUsageStore {
    db: Arc<Mutex<UsageDb>>,
    tx: Mutex<Option<std::sync::mpsc::Sender<UsageRow>>>,
    writer: Mutex<Option<std::thread::JoinHandle<()>>>,
    counters: Arc<Counters>,
    path: Option<String>,
}

impl KeyUsageStore {
    /// Open the durable store at `path` and start its writer thread.
    pub fn open(path: &Path) -> Result<Arc<Self>, UsageError> {
        let db = UsageDb::open(path)?;
        Self::start(db, Some(path.display().to_string()))
    }

    /// An ephemeral store (tests, or a daemon with no durable location).
    pub fn in_memory() -> Result<Arc<Self>, UsageError> {
        let db = UsageDb::open_in_memory()?;
        Self::start(db, None)
    }

    fn start(db: UsageDb, path: Option<String>) -> Result<Arc<Self>, UsageError> {
        let db = Arc::new(Mutex::new(db));
        let (tx, rx) = std::sync::mpsc::channel::<UsageRow>();
        let counters = Arc::new(Counters::default());
        // A store with no writer would swallow every row in silence, so a
        // failed spawn makes the store UNAVAILABLE (the caller degrades
        // loudly) instead of quietly metering nothing.
        let writer = std::thread::Builder::new()
            .name("llmux-usage-writer".into())
            .spawn({
                let db = db.clone();
                let counters = counters.clone();
                move || writer_loop(db, rx, counters)
            })
            .map_err(|source| UsageError::Io {
                path: path.clone().unwrap_or_else(|| ":memory:".into()),
                source,
            })?;
        Ok(Arc::new(Self {
            db,
            tx: Mutex::new(Some(tx)),
            writer: Mutex::new(Some(writer)),
            counters,
            path,
        }))
    }

    /// Queue one row for durable storage. Never blocks, never touches the
    /// disk; a full queue drops the row and counts it.
    pub fn record(&self, row: UsageRow) {
        if self.counters.queued.load(Ordering::Relaxed) >= WRITE_QUEUE_MAX {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let guard = lock(&self.tx);
        if let Some(tx) = guard.as_ref() {
            self.counters.queued.fetch_add(1, Ordering::Relaxed);
            if tx.send(row).is_err() {
                self.counters.queued.fetch_sub(1, Ordering::Relaxed);
                self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Run one bounded aggregate query (blocking — `spawn_blocking`).
    pub fn query(&self, query: &UsageQuery) -> Result<UsageReport, UsageError> {
        lock(&self.db).query(query)
    }

    /// Import a legacy activity-log prefix (blocking — `spawn_blocking`).
    ///
    /// Chunked so a HALF-GIGABYTE `activity.jsonl` cannot freeze the daemon:
    /// the store lock is taken only to resolve the next range and to commit a
    /// chunk, never across the read+parse, so live writes and admin queries
    /// interleave with the migration. Aggregates served while this runs are
    /// partial BY CONSTRUCTION and say so ([`UsageHealth::importing`]).
    pub fn import_activity_jsonl(
        &self,
        path: &Path,
        up_to: u64,
    ) -> Result<ImportOutcome, UsageError> {
        self.counters.importing.store(true, Ordering::Relaxed);
        let outcome = self.import_chunked(path, up_to);
        self.counters.import_pct.store(100, Ordering::Relaxed);
        self.counters.importing.store(false, Ordering::Relaxed);
        // A failed migration leaves PARTIAL history behind. Recording it in
        // the health block is what keeps that partial answer from reading as a
        // complete one on every later query — a log line nobody reads cannot.
        if let Err(err) = &outcome {
            self.counters.errors.fetch_add(1, Ordering::Relaxed);
            *lock(&self.counters.last_error) = Some(err.to_string());
        }
        outcome
    }

    fn import_chunked(&self, path: &Path, up_to: u64) -> Result<ImportOutcome, UsageError> {
        let mut outcome = ImportOutcome::default();
        loop {
            // Short lock: stat + offset/witness only.
            let range = lock(&self.db).import_range(path, up_to)?;
            let Some((offset, end)) = range else {
                break;
            };
            // NO lock held here — this is the expensive part.
            let chunk = read_chunk(path, offset, end, IMPORT_CHUNK)?;
            if chunk.bytes == 0 {
                break; // torn trailing record: leave it for the next pass
            }
            let inserted = lock(&self.db).commit_import(path, &chunk.rows, offset + chunk.bytes)?;
            outcome.imported += inserted;
            outcome.duplicates += chunk.rows.len() as u64 - inserted;
            outcome.skipped += chunk.skipped;
            outcome.bytes += chunk.bytes;
            let done = offset + chunk.bytes;
            let pct = if end > 0 {
                ((done.min(end) as u128 * 100) / end.max(1) as u128) as u8
            } else {
                100
            };
            self.counters.import_pct.store(pct, Ordering::Relaxed);
        }
        Ok(outcome)
    }

    /// Stored row count (blocking).
    pub fn row_count(&self) -> Result<u64, UsageError> {
        lock(&self.db).row_count()
    }

    /// Point-in-time health snapshot.
    pub fn health(&self) -> UsageHealth {
        UsageHealth {
            written: self.counters.written.load(Ordering::Relaxed),
            dropped: self.counters.dropped.load(Ordering::Relaxed),
            errors: self.counters.errors.load(Ordering::Relaxed),
            importing: self.counters.importing.load(Ordering::Relaxed),
            import_pct: self.counters.import_pct.load(Ordering::Relaxed),
            last_error: lock(&self.counters.last_error).clone(),
            path: self.path.clone(),
        }
    }
}

impl Drop for KeyUsageStore {
    fn drop(&mut self) {
        // Close the queue, then let the writer finish what it holds: a daemon
        // shutting down must not lose the last seconds of metering.
        lock(&self.tx).take();
        if let Some(handle) = lock(&self.writer).take() {
            let _ = handle.join();
        }
    }
}

/// Drain the queue into batched transactions until every sender is gone.
fn writer_loop(
    db: Arc<Mutex<UsageDb>>,
    rx: std::sync::mpsc::Receiver<UsageRow>,
    counters: Arc<Counters>,
) {
    while let Ok(first) = rx.recv() {
        // Coalesce whatever else is already queued into one transaction.
        let mut batch = vec![first];
        while let Ok(row) = rx.try_recv() {
            batch.push(row);
            if batch.len() >= IMPORT_CHUNK {
                break;
            }
        }
        let result = lock(&db).insert_many(&batch);
        counters
            .queued
            .fetch_sub(batch.len() as u64, Ordering::Relaxed);
        match result {
            Ok(inserted) => {
                counters.written.fetch_add(inserted, Ordering::Relaxed);
            }
            Err(err) => {
                counters.errors.fetch_add(1, Ordering::Relaxed);
                *lock(&counters.last_error) = Some(err.to_string());
                tracing::warn!(error = %err, "keys usage write failed");
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// Wire document
// ---------------------------------------------------------------------------

/// The served/rendered answer to one keys-usage query: the filtered tenant
/// rows (named + priced server-side, exactly like the dashboard document does
/// it, because an attach client has neither the key registry nor the pricing
/// overrides) plus the filter metadata that keeps a filtered view from ever
/// being relabelled as lifetime data.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KeysUsageDoc {
    /// Applied window label (`all`/`1h`/…).
    pub window: String,
    /// Applied model filter (empty = all models).
    pub models: Vec<String>,
    /// Models observed in the window — what the picker offers.
    pub available_models: Vec<String>,
    /// The exact closed interval the rows come from (`from_ms = 0` for `all`).
    pub from_ms: u64,
    pub to_ms: u64,
    /// Matched request count.
    pub rows: u64,
    /// When the answer was computed (epoch ms).
    pub generated_ms: u64,
    pub tenants: Vec<TenantUsageDoc>,
    /// Store health at answer time (write errors are visible, not hidden).
    #[serde(default)]
    pub health: UsageHealth,
}

/// Join a store report with the key registry (display names/emails) and the
/// pricing overrides. The ONE place tenant rows are named and priced, shared
/// by the HTTP endpoint and the in-process TUI so both render identical rows.
pub fn usage_doc(
    report: UsageReport,
    keys: &[KeyRowDoc],
    overrides: &HashMap<String, crate::pricing::ModelPrice>,
    health: UsageHealth,
    generated_ms: u64,
) -> KeysUsageDoc {
    let tenants = report
        .tenants
        .into_iter()
        .map(|t| {
            let key = keys.iter().find(|k| k.id == t.tenant);
            let models: Vec<TenantModelDoc> = t
                .models
                .into_iter()
                .map(|m| {
                    let tokens = TokenCounts {
                        input: m.tokens_in,
                        output: m.tokens_out,
                        cache_read: Some(m.cache_read),
                        cache_creation: Some(m.cache_creation),
                    };
                    TenantModelDoc {
                        cost_usd: crate::pricing::cost_usd(&m.group, &m.model, &tokens, overrides),
                        group: m.group,
                        model: m.model,
                        requests: m.requests,
                        tokens_in: m.tokens_in,
                        tokens_out: m.tokens_out,
                        cache_read: m.cache_read,
                        cache_creation: m.cache_creation,
                    }
                })
                .collect();
            TenantUsageDoc {
                tenant: t.tenant.clone(),
                name: key.map(|k| k.name.clone()).unwrap_or(t.tenant),
                email: key.and_then(|k| k.email.clone()),
                requests: t.requests,
                ok: t.ok,
                errors: t.errors,
                tokens_in: t.tokens_in,
                tokens_out: t.tokens_out,
                cost_usd: models.iter().map(|m| m.cost_usd).sum(),
                first_ms: t.first_ms,
                last_ms: t.last_ms,
                models,
            }
        })
        .collect();
    KeysUsageDoc {
        window: report.window.as_str().to_string(),
        models: report.models,
        available_models: report.available_models,
        from_ms: report.from_ms,
        to_ms: report.to_ms,
        rows: report.rows,
        generated_ms,
        tenants,
        health,
    }
}

// ---------------------------------------------------------------------------
// Location
// ---------------------------------------------------------------------------

/// Resolve the durable store's location from the daemon's config path.
///
/// - The DEFAULT config location gets the per-CHANNEL directory beside it:
///   `~/.config/llmux/usage.sqlite3`, or `~/.config/llmux-preview/…` for a
///   preview build — the two channels never share one history.
/// - Any OTHER config path (an explicit `$LLMUX_CONFIG`, a test tempdir) gets
///   an isolated sibling directory named after the config file, so an
///   alternate config can never read or write the user's real store.
/// - No config path (persistence disabled) → no durable store.
pub fn db_path_for(
    config_path: Option<&Path>,
    default_config_path: Option<&Path>,
    channel: &str,
) -> Option<PathBuf> {
    let config_path = config_path?;
    let parent = config_path.parent().unwrap_or(Path::new("."));
    let dir = if Some(config_path) == default_config_path {
        parent.join(channel_dir(channel))
    } else {
        parent.join(config_path.file_stem()?)
    };
    Some(dir.join("usage.sqlite3"))
}

/// [`db_path_for`] against the running binary's channel and the platform's
/// default config location.
pub fn db_path_for_config(config_path: Option<&Path>) -> Option<PathBuf> {
    // The DEFAULT location, not the env-resolved one: `$LLMUX_CONFIG` must
    // compare UNEQUAL here so an override lands in its own sibling directory.
    let default = crate::config::xdg_config_dir().map(|dir| dir.join("llmux.json"));
    db_path_for(
        config_path,
        default.as_deref(),
        crate::build_info::BUILD_CHANNEL,
    )
}

fn channel_dir(channel: &str) -> &'static str {
    match channel {
        "preview" => "llmux-preview",
        _ => "llmux",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "llmux-key-usage-{}-{}",
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

    fn line(ts_ms: u64, id: u64, tenant: &str) -> String {
        serde_json::json!({
            "v": 1, "ts_ms": ts_ms, "id": id, "method": "POST", "path": "/v1/messages",
            "account": "a", "status": 200, "duration_ms": 1,
            "tokens": { "input": 1, "output": 1 },
            "group": "claude", "model": "m1", "tenant": tenant
        })
        .to_string()
            + "\n"
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open");
        f.write_all(text.as_bytes()).expect("append");
    }

    fn len(path: &Path) -> u64 {
        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    }

    /// K-4 crash window: the rotation RESET (offset back to 0) and the new
    /// source witness must land together. Persisting the witness first and the
    /// offset only with the first committed chunk means a crash in between
    /// leaves "new file, old offset" — and the head of that file is then
    /// skipped forever. Simulated by resolving the range (which performs the
    /// reset) and dying before any commit.
    #[test]
    fn a_rotation_reset_survives_a_crash_before_the_first_chunk() {
        let tmp = TempDir::new();
        let jsonl = tmp.path().join("activity.jsonl");
        let db_path = tmp.path().join("usage.sqlite3");
        let mut db = UsageDb::open(&db_path).expect("open");
        for i in 0..4 {
            append(&jsonl, &line(1_000 + i, i, "k-a"));
        }
        assert_eq!(
            db.import_activity_jsonl(&jsonl, len(&jsonl))
                .expect("import")
                .imported,
            4
        );

        // Replacement: a NEW file (new inode) of the same length, different
        // requests.
        std::fs::remove_file(&jsonl).expect("rotate");
        for i in 0..4 {
            append(&jsonl, &line(9_000 + i, 90 + i, "k-b"));
        }

        // "Crash" right after the range is resolved: the reset is persisted,
        // nothing is committed.
        let range = db.import_range(&jsonl, len(&jsonl)).expect("range");
        assert_eq!(
            range,
            Some((0, len(&jsonl))),
            "the new source restarts at 0"
        );
        drop(db);

        // Next boot: the whole replacement file must still be imported.
        let mut db = UsageDb::open(&db_path).expect("reopen");
        let outcome = db
            .import_activity_jsonl(&jsonl, len(&jsonl))
            .expect("import after crash");
        assert_eq!(
            outcome.imported, 4,
            "the head of the replacement file is not skipped"
        );
        assert_eq!(db.row_count().expect("count"), 8);
    }

    /// K-4: `copytruncate` that refills the file to at least its previous
    /// length, in place (SAME inode). Neither the byte offset nor the file id
    /// notices, so the source witness also covers the file's HEAD.
    #[test]
    fn an_in_place_refill_of_equal_length_is_re_read() {
        let tmp = TempDir::new();
        let jsonl = tmp.path().join("activity.jsonl");
        let mut db = UsageDb::open(&tmp.path().join("usage.sqlite3")).expect("open");
        for i in 0..4 {
            append(&jsonl, &line(1_000 + i, 10 + i, "k-a"));
        }
        let original = len(&jsonl);
        db.import_activity_jsonl(&jsonl, original).expect("import");
        assert_eq!(db.row_count().unwrap(), 4);

        // Truncate and refill IN PLACE with different requests of the same
        // total length (the inode never changes).
        std::fs::write(&jsonl, b"").expect("truncate");
        for i in 0..4 {
            append(&jsonl, &line(9_000 + i, 90 + i, "k-b"));
        }
        assert_eq!(len(&jsonl), original, "the refill matches the old length");

        let outcome = db
            .import_activity_jsonl(&jsonl, len(&jsonl))
            .expect("import after refill");
        assert_eq!(outcome.imported, 4, "the refilled content is read");
        assert_eq!(db.row_count().unwrap(), 8);
    }
}
