//! Past-day backups of the keys-usage statistics (`usage.sqlite3`), the same
//! storage contract as raw-io and soma-work metrics: SQLite is the live
//! store, every finished local day is exported to a verified long-window
//! zstd jsonl file, and nothing is ever deleted.
//!
//! Layout: `<dir of usage.sqlite3>/usage-archive/usage-YYYY-MM-DD[.N].jsonl.zst`
//! plus a `.manifest.json` (rows, sha256 of the plain lines, fingerprint). A
//! day whose rows change after it was exported (late events, re-imports)
//! gets the next generation; earlier generations stay.
//!
//! The live database is only read, one day per short statement on the
//! `usage_ts_idx` range of that day, so no read transaction is held while a
//! day is compressed or verified and the metering writer is never starved.
//! Rows are insert-only (`INSERT OR IGNORE`), so a day whose
//! `count:sum(ts_ms):max(ts_ms)` fingerprint equals its latest manifest's is
//! skipped without reading its rows. One exporter at a time per directory
//! (`usage-archive/.lock`, also across processes during a rolling restart).

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::raw_store::{day_key, StoreError};

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;
/// A day's guessed UTC bounds are widened by this much on both sides; the
/// exact local-day filter (`date(..., 'localtime')`) does the rest, so a DST
/// switch inside the day never drops or doubles a row.
const BRACKET_MS: i64 = 3 * HOUR_MS;

/// Sidecar of one `usage-DAY[.N].jsonl.zst`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageManifest {
    pub day: String,
    pub generation: u32,
    pub rows: u64,
    pub plain_bytes: u64,
    pub plain_sha256: String,
    pub archive_bytes: u64,
    /// `count:sum(ts_ms):max(ts_ms)` of the day's rows when exported.
    #[serde(default)]
    pub fingerprint: String,
    pub zstd: String,
    pub created_ms: u64,
}

/// `<db dir>/usage-archive`.
pub fn archive_dir_for(db: &Path) -> PathBuf {
    db.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("usage-archive")
}

fn name(day: &str, generation: u32, ext: &str) -> String {
    if generation == 0 {
        format!("usage-{day}.{ext}")
    } else {
        format!("usage-{day}.{generation}.{ext}")
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Latest manifest of `day` and the next free generation number. A data
/// file without a manifest (crash between the two renames) still occupies
/// its generation, so it is never overwritten.
fn latest(dir: &Path, day: &str) -> (Option<UsageManifest>, u32) {
    let mut generation = 0u32;
    let mut last = None;
    loop {
        let m = dir.join(name(day, generation, "manifest.json"));
        let z = dir.join(name(day, generation, "jsonl.zst"));
        if !m.exists() && !z.exists() {
            return (last, generation);
        }
        if let Some(parsed) = fs::read_to_string(&m)
            .ok()
            .and_then(|t| serde_json::from_str::<UsageManifest>(&t).ok())
        {
            last = Some(parsed);
        }
        generation += 1;
    }
}

/// Local day number (days since 1970-01-01 in the host's zone) of `ts_ms`.
fn local_day_number(ts_ms: i64) -> i64 {
    let at = UNIX_EPOCH + Duration::from_millis(u64::try_from(ts_ms).unwrap_or(0));
    let offset = crate::tui::format::local_offset_secs(at);
    (ts_ms.div_euclid(1000) + offset).div_euclid(86_400)
}

/// `[lo, hi)` epoch-ms range that contains every instant of local day `n`.
fn bracket(n: i64) -> (i64, i64) {
    let utc_midnight = n.saturating_mul(DAY_MS);
    let at = UNIX_EPOCH + Duration::from_millis(u64::try_from(utc_midnight).unwrap_or(0));
    let offset_ms = crate::tui::format::local_offset_secs(at) * 1000;
    let start = utc_midnight - offset_ms;
    (start - BRACKET_MS, start + DAY_MS + BRACKET_MS)
}

const DAY_FILTER: &str = "ts_ms >= ?1 AND ts_ms < ?2 \
     AND date(ts_ms / 1000, 'unixepoch', 'localtime') = ?3";

/// Export every local day before `today` whose rows differ from its latest
/// export. Returns `(day, generation, rows)` of each file written.
/// `Err(Busy)` = another exporter holds the directory, or `stop()` became
/// true (shutdown); days already published stay published.
pub fn export_usage_days(
    db: &Path,
    today: &str,
    stop: &dyn Fn() -> bool,
) -> Result<Vec<(String, u32, u64)>, StoreError> {
    let dir = archive_dir_for(db);
    fs::create_dir_all(&dir)?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => {
            return Err(StoreError::Busy("another usage exporter is running".into()))
        }
        Err(fs::TryLockError::Error(e)) => return Err(e.into()),
    }
    // Read-write open (never CREATE) + query_only: a read-only handle cannot
    // map the -shm of a WAL database the daemon does not have open.
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(10))?;
    conn.pragma_update(None, "query_only", true)?;
    // Only a WAL database is read: there a reader never blocks a commit. In
    // rollback-journal mode even a short read can fail a writer that has no
    // busy timeout (an older daemon still draining), losing its batch. The
    // daemon switches the file to WAL when it opens it.
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::Busy(format!(
            "usage.sqlite3 is in {mode} mode, export waits for WAL"
        )));
    }
    let first: Option<i64> = conn.query_row("SELECT min(ts_ms) FROM usage", [], |r| r.get(0))?;
    let Some(first) = first else {
        return Ok(Vec::new());
    };
    let mut written = Vec::new();
    let mut n = local_day_number(first);
    loop {
        let day = day_key(n);
        if day.as_str() >= today {
            return Ok(written);
        }
        if stop() {
            return Err(StoreError::Busy("shutdown".into()));
        }
        let (lo, hi) = bracket(n);
        let (count, sum_ts, max_ts): (i64, i64, i64) = conn.query_row(
            &format!(
                "SELECT count(*), coalesce(sum(ts_ms), 0), coalesce(max(ts_ms), 0) \
                   FROM usage WHERE {DAY_FILTER}"
            ),
            rusqlite::params![lo, hi, day],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if count > 0 {
            let fingerprint = format!("{count}:{sum_ts}:{max_ts}");
            let (last, generation) = latest(&dir, &day);
            if last.as_ref().is_none_or(|m| m.fingerprint != fingerprint) {
                let lines = read_day(&conn, lo, hi, &day, stop)?;
                if let Some(rows) =
                    publish_day(&dir, &day, generation, last.as_ref(), &lines, fingerprint)?
                {
                    written.push((day, generation, rows));
                }
            }
        }
        // Next day with any row: one index seek, so long idle gaps cost
        // nothing. Starts before this day's guessed end so the first rows
        // of the next day are never stepped over.
        let next: Option<i64> = conn
            .query_row(
                "SELECT ts_ms FROM usage WHERE ts_ms >= ?1 ORDER BY ts_ms LIMIT 1",
                [hi - 2 * BRACKET_MS],
                |r| r.get(0),
            )
            .optional()?;
        match next {
            Some(ts) => n = (n + 1).max(local_day_number(ts)),
            None => return Ok(written),
        }
    }
}

/// One day's rows as jsonl lines, in one short statement (dropped before
/// the caller compresses anything).
fn read_day(
    conn: &Connection,
    lo: i64,
    hi: i64,
    day: &str,
    stop: &dyn Fn() -> bool,
) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT json_object('event_key', event_key, 'ts_ms', ts_ms, 'tenant', tenant,
                            'grp', grp, 'model', model, 'status', status,
                            'tokens_in', tokens_in, 'tokens_out', tokens_out,
                            'cache_read', cache_read, 'cache_creation', cache_creation)
           FROM usage WHERE {DAY_FILTER} ORDER BY ts_ms, event_key"
    ))?;
    let mut rows = stmt.query(rusqlite::params![lo, hi, day])?;
    let mut lines = Vec::new();
    while let Some(row) = rows.next()? {
        lines.push(row.get::<_, String>(0)?);
        if lines.len() % 4096 == 0 && stop() {
            return Err(StoreError::Busy("shutdown".into()));
        }
    }
    Ok(lines)
}

/// Write, decode-verify and publish one day. `Ok(None)` = byte-identical to
/// its latest export (only the fingerprint was missing), nothing written.
fn publish_day(
    dir: &Path,
    day: &str,
    generation: u32,
    last: Option<&UsageManifest>,
    lines: &[String],
    fingerprint: String,
) -> Result<Option<u64>, StoreError> {
    let mut body = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
    for l in lines {
        body.push_str(l);
        body.push('\n');
    }
    let sha = hex(&Sha256::digest(body.as_bytes()));
    if last.is_some_and(|m| m.plain_sha256 == sha) {
        return Ok(None);
    }
    let path = dir.join(name(day, generation, "jsonl.zst"));
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    {
        let mut enc = zstd::stream::write::Encoder::new(File::create(&tmp)?, 3)?;
        enc.long_distance_matching(true)?;
        enc.window_log(27)?;
        enc.include_checksum(true)?;
        enc.write_all(body.as_bytes())?;
        enc.finish()?.sync_all()?;
    }
    // Verify the file on disk before publishing it.
    let mut dec = zstd::stream::read::Decoder::new(File::open(&tmp)?)?;
    dec.window_log_max(31)?;
    let mut reader = BufReader::new(dec);
    let mut hasher = Sha256::new();
    let mut n = 0u64;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        hasher.update(&buf);
        n += 1;
    }
    if hex(&hasher.finalize()) != sha || n != lines.len() as u64 {
        // Our own unpublished temp file, not data: the rows are still in
        // the live database and the day is retried next cycle.
        let _ = fs::remove_file(&tmp);
        return Err(StoreError::Verify {
            day: day.to_string(),
            detail: "usage export did not round-trip".into(),
        });
    }
    fs::rename(&tmp, &path)?;
    let manifest = UsageManifest {
        day: day.to_string(),
        generation,
        rows: n,
        plain_bytes: body.len() as u64,
        plain_sha256: sha,
        archive_bytes: fs::metadata(&path)?.len(),
        fingerprint,
        zstd: "level 3, long window; decode: zstd -d --long=31".into(),
        created_ms: super::raw_io::now_ms(),
    };
    let final_manifest = dir.join(name(day, generation, "manifest.json"));
    let mtmp = PathBuf::from(format!("{}.tmp", final_manifest.display()));
    fs::write(&mtmp, serde_json::to_vec_pretty(&manifest)?)?;
    File::open(&mtmp)?.sync_all()?;
    fs::rename(&mtmp, final_manifest)?;
    Ok(Some(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::raw_store::{count_lines_zst, day_of};

    fn db_with(rows: &[(&str, u64)]) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "llmux-usage-archive-{}-{}",
            std::process::id(),
            ulid::Ulid::new()
        ));
        fs::create_dir_all(&dir).expect("dir");
        let db = dir.join("usage.sqlite3");
        let c = Connection::open(&db).expect("open");
        c.execute_batch(
            "CREATE TABLE usage (event_key TEXT PRIMARY KEY, ts_ms INTEGER NOT NULL,
               tenant TEXT NOT NULL, grp TEXT, model TEXT, status INTEGER NOT NULL,
               tokens_in INTEGER NOT NULL DEFAULT 0, tokens_out INTEGER NOT NULL DEFAULT 0,
               cache_read INTEGER NOT NULL DEFAULT 0, cache_creation INTEGER NOT NULL DEFAULT 0);
             CREATE INDEX usage_ts_idx ON usage(ts_ms);
             PRAGMA journal_mode=WAL;",
        )
        .expect("schema");
        for (k, ts) in rows {
            c.execute(
                "INSERT INTO usage (event_key, ts_ms, tenant, grp, model, status, tokens_in)
                 VALUES (?1, ?2, 't', 'claude', 'm', 200, 5)",
                rusqlite::params![k, *ts as i64],
            )
            .expect("row");
        }
        (dir, db)
    }

    const DAY: u64 = 86_400_000;
    const T0: u64 = 1_793_070_000_000; // 2026-10-27T03:00Z
    const NEVER: &dyn Fn() -> bool = &|| false;

    #[test]
    fn past_days_are_exported_once_and_changes_get_a_new_generation() {
        let (dir, db) = db_with(&[
            ("a", T0),
            ("b", T0 + 1000),
            ("c", T0 + DAY),
            ("d", T0 + 3 * DAY),
        ]);
        let day0 = day_of(T0);
        let today = day_of(T0 + 3 * DAY);
        let out = export_usage_days(&db, &today, NEVER).expect("export");
        assert_eq!(out.len(), 2, "two finished days, today excluded");
        assert_eq!(out[0], (day0.clone(), 0, 2));
        assert!(
            export_usage_days(&db, &today, NEVER)
                .expect("again")
                .is_empty(),
            "unchanged"
        );
        // A late event for day 0: generation 1, generation 0 kept.
        let c = Connection::open(&db).expect("open");
        c.execute(
            "INSERT INTO usage (event_key, ts_ms, tenant, status) VALUES ('late', ?1, 't', 200)",
            [(T0 + 2000) as i64],
        )
        .expect("late");
        let out = export_usage_days(&db, &today, NEVER).expect("export");
        assert_eq!(out, vec![(day0.clone(), 1, 3)]);
        let ad = archive_dir_for(&db);
        assert!(ad.join(format!("usage-{day0}.jsonl.zst")).exists());
        assert_eq!(
            count_lines_zst(&ad.join(format!("usage-{day0}.1.jsonl.zst")), &|| false)
                .expect("count"),
            3
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// Every row lands in exactly the local day `day_of` gives it, across
    /// midnights and an idle gap, and today stays live.
    #[test]
    fn rows_are_bucketed_by_local_day_across_midnights_and_gaps() {
        let mut rows = Vec::new();
        let mut ts = T0;
        while ts < T0 + 8 * DAY {
            rows.push((format!("k{ts}"), ts));
            ts += 37 * 60_000;
        }
        // Crosses the US DST end (2026-11-01); then idle days, one more
        // active day and "today".
        rows.push(("late1".into(), T0 + 14 * DAY));
        rows.push(("late2".into(), T0 + 14 * DAY + 60_000));
        rows.push(("now".into(), T0 + 16 * DAY));
        let refs: Vec<(&str, u64)> = rows.iter().map(|(k, t)| (k.as_str(), *t)).collect();
        let (dir, db) = db_with(&refs);
        let today = day_of(T0 + 16 * DAY);
        let mut expected = std::collections::BTreeMap::<String, u64>::new();
        for (_, t) in &rows {
            let d = day_of(*t);
            if d < today {
                *expected.entry(d).or_default() += 1;
            }
        }
        let out = export_usage_days(&db, &today, NEVER).expect("export");
        let got: std::collections::BTreeMap<String, u64> =
            out.iter().map(|(d, _, n)| (d.clone(), *n)).collect();
        assert_eq!(got, expected);
        let ad = archive_dir_for(&db);
        for (d, n) in &expected {
            assert_eq!(
                count_lines_zst(&ad.join(format!("usage-{d}.jsonl.zst")), &|| false)
                    .expect("count"),
                *n
            );
            let m: UsageManifest = serde_json::from_slice(
                &fs::read(ad.join(format!("usage-{d}.manifest.json"))).expect("manifest"),
            )
            .expect("parse");
            assert_eq!(m.rows, *n);
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_rollback_journal_database_is_never_read() {
        let (dir, db) = db_with(&[("a", T0)]);
        Connection::open(&db)
            .expect("open")
            .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get::<_, String>(0))
            .expect("delete mode");
        let today = day_of(T0 + DAY);
        assert!(matches!(
            export_usage_days(&db, &today, NEVER),
            Err(StoreError::Busy(_))
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_held_lock_defers_and_stop_cancels_without_partial_files() {
        let (dir, db) = db_with(&[("a", T0), ("b", T0 + DAY), ("c", T0 + 2 * DAY)]);
        let today = day_of(T0 + 2 * DAY);
        let ad = archive_dir_for(&db);
        fs::create_dir_all(&ad).expect("dir");
        let held = File::create(ad.join(".lock")).expect("lock file");
        held.lock().expect("hold");
        assert!(matches!(
            export_usage_days(&db, &today, NEVER),
            Err(StoreError::Busy(_))
        ));
        held.unlock().expect("release");
        assert!(matches!(
            export_usage_days(&db, &today, &|| true),
            Err(StoreError::Busy(_))
        ));
        let leftovers: Vec<_> = fs::read_dir(&ad)
            .expect("ls")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != ".lock")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        assert_eq!(
            export_usage_days(&db, &today, NEVER).expect("export").len(),
            2
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// Real-data smoke run: `LLMUX_USAGE_DB=/path/to/copy.sqlite3 cargo test
    /// --lib usage_archive -- --ignored` (exports next to the copy).
    #[test]
    #[ignore = "needs LLMUX_USAGE_DB"]
    fn smoke_export_real_db() {
        let Some(db) = std::env::var_os("LLMUX_USAGE_DB") else {
            return;
        };
        let today = day_of(crate::proxy::raw_io::now_ms());
        let out = export_usage_days(Path::new(&db), &today, NEVER).expect("export");
        let rows: u64 = out.iter().map(|(_, _, n)| n).sum();
        println!("exported {} days, {rows} rows", out.len());
    }
}
