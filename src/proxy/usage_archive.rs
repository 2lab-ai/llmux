//! Past-day backups of the keys-usage statistics (`usage.sqlite3`), the same
//! storage contract as raw-io and soma-work metrics: SQLite is the live
//! store, every finished local day is exported once to a verified
//! long-window zstd jsonl file, and nothing is ever deleted.
//!
//! Layout: `<dir of usage.sqlite3>/usage-archive/usage-YYYY-MM-DD[.N].jsonl.zst`
//! plus a `.manifest.json` (rows, sha256 of the plain lines). A day whose rows
//! change after it was exported (late events, re-imports) gets the next
//! generation; earlier generations stay. The live database is never touched
//! (opened read-only).

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

use super::raw_store::{ArchiveManifest, StoreError};

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

/// Latest manifest of `day` and the next free generation number.
fn latest(dir: &Path, day: &str) -> (Option<ArchiveManifest>, u32) {
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
            .and_then(|t| serde_json::from_str::<ArchiveManifest>(&t).ok())
        {
            last = Some(parsed);
        }
        generation += 1;
    }
}

/// Export every local day before `today` whose current rows differ from its
/// latest export. Returns `(day, generation, rows)` of each file written.
pub fn export_usage_days(db: &Path, today: &str) -> Result<Vec<(String, u32, u64)>, StoreError> {
    let dir = archive_dir_for(db);
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(10))?;
    let mut stmt = conn.prepare(
        "SELECT date(ts_ms / 1000, 'unixepoch', 'localtime') AS day,
                json_object('event_key', event_key, 'ts_ms', ts_ms, 'tenant', tenant,
                            'grp', grp, 'model', model, 'status', status,
                            'tokens_in', tokens_in, 'tokens_out', tokens_out,
                            'cache_read', cache_read, 'cache_creation', cache_creation)
           FROM usage
          WHERE date(ts_ms / 1000, 'unixepoch', 'localtime') < ?1
          ORDER BY day, ts_ms, event_key",
    )?;
    let mut rows = stmt.query([today])?;
    let mut written = Vec::new();
    let mut cur: Option<(String, Vec<String>)> = None;
    let mut flush = |day: String, lines: Vec<String>| -> Result<(), StoreError> {
        let mut body = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
        for l in &lines {
            body.push_str(l);
            body.push('\n');
        }
        let sha = hex(&Sha256::digest(body.as_bytes()));
        let (last, generation) = latest(&dir, &day);
        if last.as_ref().is_some_and(|m| m.plain_sha256 == sha) {
            return Ok(()); // unchanged since its last export
        }
        fs::create_dir_all(&dir)?;
        let path = dir.join(name(&day, generation, "jsonl.zst"));
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
            let _ = fs::remove_file(&tmp);
            return Err(StoreError::Verify {
                day,
                detail: "usage export did not round-trip".into(),
            });
        }
        fs::rename(&tmp, &path)?;
        let manifest = ArchiveManifest {
            day: day.clone(),
            generation,
            rows: n,
            plain_bytes: body.len() as u64,
            plain_sha256: sha,
            archive_bytes: fs::metadata(&path)?.len(),
            src_counts: Default::default(),
            zstd: "level 3, long window; decode: zstd -d --long=31".into(),
            created_ms: super::raw_io::now_ms(),
        };
        let mtmp = dir.join(format!("{}.tmp", name(&day, generation, "manifest.json")));
        fs::write(&mtmp, serde_json::to_vec_pretty(&manifest)?)?;
        fs::rename(&mtmp, dir.join(name(&day, generation, "manifest.json")))?;
        written.push((day, generation, n));
        Ok(())
    };
    while let Some(row) = rows.next()? {
        let day: String = row.get(0)?;
        let line: String = row.get(1)?;
        match cur.as_mut() {
            Some((d, lines)) if *d == day => lines.push(line),
            _ => {
                if let Some((d, lines)) = cur.take() {
                    flush(d, lines)?;
                }
                cur = Some((day, vec![line]));
            }
        }
    }
    if let Some((d, lines)) = cur.take() {
        flush(d, lines)?;
    }
    drop(flush);
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

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
               cache_read INTEGER NOT NULL DEFAULT 0, cache_creation INTEGER NOT NULL DEFAULT 0);",
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

    #[test]
    fn past_days_are_exported_once_and_changes_get_a_new_generation() {
        let (dir, db) = db_with(&[
            ("a", T0),
            ("b", T0 + 1000),
            ("c", T0 + DAY),
            ("d", T0 + 3 * DAY),
        ]);
        let day0 = crate::proxy::raw_store::day_of(T0);
        let today = crate::proxy::raw_store::day_of(T0 + 3 * DAY);
        let out = export_usage_days(&db, &today).expect("export");
        assert_eq!(out.len(), 2, "two finished days, today excluded");
        assert_eq!(out[0], (day0.clone(), 0, 2));
        assert!(
            export_usage_days(&db, &today).expect("again").is_empty(),
            "unchanged"
        );
        // A late event for day 0: generation 1, generation 0 kept.
        let c = Connection::open(&db).expect("open");
        c.execute(
            "INSERT INTO usage (event_key, ts_ms, tenant, status) VALUES ('late', ?1, 't', 200)",
            [(T0 + 2000) as i64],
        )
        .expect("late");
        let out = export_usage_days(&db, &today).expect("export");
        assert_eq!(out, vec![(day0.clone(), 1, 3)]);
        let ad = archive_dir_for(&db);
        assert!(ad.join(format!("usage-{day0}.jsonl.zst")).exists());
        assert_eq!(
            crate::proxy::raw_store::count_lines_zst(
                &ad.join(format!("usage-{day0}.1.jsonl.zst")),
                &|| false
            )
            .expect("count"),
            3
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
        let today = crate::proxy::raw_store::day_of(crate::proxy::raw_io::now_ms());
        let out = export_usage_days(Path::new(&db), &today).expect("export");
        let rows: u64 = out.iter().map(|(_, _, n)| n).sum();
        println!("exported {} days, {rows} rows", out.len());
    }
}
