//! Acceptance for the durable keys-usage store (`docs/keys-history/spec.md`,
//! trace K): the SQLite usage rows behind the `K` panel, their exact rolling
//! windows + model filters, the one-time legacy `activity.jsonl` import, and
//! the admin `GET /llmux/keys/usage` endpoint.
//!
//! Isolation: every test owns a tempdir. The store is opened at a tempdir path
//! and every daemon here runs with `config_path` pointed INTO that tempdir —
//! the DB location is derived from the config path
//! ([`llmux::key_usage::db_path_for_config`]), so no test can open, read or
//! write the user's real `~/.config/llmux/usage.sqlite3`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use llmux::config::{self, ClientKey, ClientKeyKind, Config};
use llmux::key_usage::{KeyUsageStore, UsageDb, UsageQuery, UsageRow, UsageWindow};
use llmux::proxy::server::{serve, AppState};
use llmux::scheduler::AccountPool;
use llmux::tui::{ActivityEvent, TokenCounts};

const ADMIN_KEY: &str = "test-admin-key";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Throwaway directory, removed on drop (same hand-rolled helper the rest of
/// the suite uses — this crate carries no dev-dependencies).
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "llmux-keys-history-{}-{}",
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

/// A usage row at `ts_ms` for `tenant`/`model`, 200 OK with 10 in / 20 out.
fn row(ts_ms: u64, id: u64, tenant: &str, model: Option<&str>) -> UsageRow {
    UsageRow::new(ts_ms, id, Some(tenant), model.map(|_| "claude"), model, 200)
        .with_tokens(10, 20, 0, 0)
}

/// A JSON line in the persisted-activity format (`activity.jsonl`).
fn jsonl_line(ts_ms: u64, id: u64, tenant: &str, model: &str, status: u16) -> String {
    serde_json::json!({
        "v": 1,
        "ts_ms": ts_ms,
        "id": id,
        "method": "POST",
        "path": "/v1/messages",
        "account": "acct",
        "status": status,
        "duration_ms": 12,
        "tokens": { "input": 10, "output": 20, "cache_read": 3, "cache_creation": 1 },
        "group": "claude",
        "model": model,
        "tenant": tenant,
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
        .expect("open jsonl");
    f.write_all(text.as_bytes()).expect("append jsonl");
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 1. Window boundaries — exact trailing [now - duration, now]
// ---------------------------------------------------------------------------

/// K-2/K-3: the 24h window is CLOSED at both ends and excludes the future.
/// Breaks if the predicate uses `>`/`<` at the edge or forgets the upper bound.
#[test]
fn window_is_closed_at_both_ends_and_excludes_the_future() {
    let now = 1_700_000_000_000_u64;
    let day = 24 * 3_600_000_u64;
    let db = UsageDb::open_in_memory().expect("open");
    db.insert(&row(now - day - 1, 1, "k-a", Some("m1")))
        .unwrap(); // just outside
    db.insert(&row(now - day, 2, "k-a", Some("m1"))).unwrap(); // exact lower edge
    db.insert(&row(now, 3, "k-a", Some("m1"))).unwrap(); // exact upper edge
    db.insert(&row(now + 1, 4, "k-a", Some("m1"))).unwrap(); // future

    let report = db
        .query(&UsageQuery::new(UsageWindow::H24, now))
        .expect("query");
    assert_eq!(report.rows, 2, "lower edge + now are in, ±1 are out");
    assert_eq!(report.from_ms, now - day);
    assert_eq!(report.to_ms, now);
}

/// K-2: `all` keeps every past row but still never counts a future stamp as
/// usage (a skewed clock must not invent history).
#[test]
fn all_window_keeps_history_but_still_drops_future_rows() {
    let now = 1_700_000_000_000_u64;
    let db = UsageDb::open_in_memory().expect("open");
    db.insert(&row(1, 1, "k-a", Some("m1"))).unwrap();
    db.insert(&row(now - 90 * 24 * 3_600_000 - 5, 2, "k-a", Some("m1")))
        .unwrap();
    db.insert(&row(now + 60_000, 3, "k-a", Some("m1"))).unwrap();

    let report = db
        .query(&UsageQuery::new(UsageWindow::All, now))
        .expect("query");
    assert_eq!(report.rows, 2, "both past rows, neither future row");
    assert_eq!(report.from_ms, 0, "all history has no lower bound");
}

/// K-2: every offered window resolves to its exact documented duration.
#[test]
fn window_durations_match_their_labels() {
    let hour = 3_600_000_u64;
    let cases = [
        (UsageWindow::All, None),
        (UsageWindow::H1, Some(hour)),
        (UsageWindow::H24, Some(24 * hour)),
        (UsageWindow::D7, Some(7 * 24 * hour)),
        (UsageWindow::D14, Some(14 * 24 * hour)),
        (UsageWindow::D30, Some(30 * 24 * hour)),
        (UsageWindow::D90, Some(90 * 24 * hour)),
    ];
    for (window, want) in cases {
        assert_eq!(window.duration_ms(), want, "{}", window.as_str());
    }
    assert_eq!(
        UsageWindow::ALL.map(|w| w.as_str()),
        ["all", "1h", "24h", "7d", "14d", "30d", "90d"],
        "the offered set and its wire labels"
    );
    assert_eq!(UsageWindow::parse("24h"), Some(UsageWindow::H24));
    assert_eq!(UsageWindow::parse("2h"), None, "unknown window is rejected");
    assert_eq!(UsageWindow::default(), UsageWindow::All, "default = all");
}

// ---------------------------------------------------------------------------
// 2. Filter intersections
// ---------------------------------------------------------------------------

/// K-3: the model filter and the window intersect (AND), never union.
#[test]
fn model_filter_intersects_with_the_window() {
    let now = 2_000_000_000_000_u64;
    let hour = 3_600_000_u64;
    let db = UsageDb::open_in_memory().expect("open");
    db.insert(&row(now - 30 * 60_000, 1, "k-a", Some("m-in")))
        .unwrap(); // in window
    db.insert(&row(now - 5 * hour, 2, "k-a", Some("m-in")))
        .unwrap(); // out of window
    db.insert(&row(now - 30 * 60_000, 3, "k-a", Some("m-other")))
        .unwrap(); // filtered out

    let report = db
        .query(&UsageQuery::new(UsageWindow::H1, now).with_models(["m-in"]))
        .expect("query");
    assert_eq!(report.rows, 1, "only the in-window row of the chosen model");
    let tenant = &report.tenants[0];
    assert_eq!(tenant.requests, 1);
    assert_eq!(tenant.models.len(), 1);
    assert_eq!(tenant.models[0].model, "m-in");
}

/// K-3: an unattributed failure (no model) stays visible under the all-models
/// query — the admin's view must account for EVERY request — but is excluded
/// once explicit models are chosen.
#[test]
fn unattributed_rows_count_under_all_models_and_drop_under_an_explicit_filter() {
    let now = 2_000_000_000_000_u64;
    let db = UsageDb::open_in_memory().expect("open");
    db.insert(&UsageRow::new(now - 1000, 1, Some("k-a"), None, None, 503))
        .unwrap();
    db.insert(&row(now - 1000, 2, "k-a", Some("m1"))).unwrap();

    let all = db
        .query(&UsageQuery::new(UsageWindow::All, now))
        .expect("query");
    let tenant = &all.tenants[0];
    assert_eq!(tenant.requests, 2, "both requests counted");
    assert_eq!(tenant.errors, 1, "the 503 is an error");
    assert_eq!(
        tenant.models.len(),
        1,
        "only the attributed row has a model"
    );

    let filtered = db
        .query(&UsageQuery::new(UsageWindow::All, now).with_models(["m1"]))
        .expect("query");
    assert_eq!(filtered.rows, 1, "the unattributed failure is excluded");
    assert_eq!(filtered.tenants[0].requests, 1);
}

/// K-6: tenant totals sum token classes and the ok/err split per window; the
/// per-model cells carry the cache classes the panel prices.
#[test]
fn tenant_totals_and_model_cells_sum_the_window() {
    let now = 2_000_000_000_000_u64;
    let db = UsageDb::open_in_memory().expect("open");
    db.insert(
        &UsageRow::new(now - 10, 1, Some("k-a"), Some("claude"), Some("m1"), 200)
            .with_tokens(10, 20, 5, 1),
    )
    .unwrap();
    db.insert(
        &UsageRow::new(now - 9, 2, Some("k-a"), Some("claude"), Some("m1"), 429)
            .with_tokens(1, 2, 0, 0),
    )
    .unwrap();
    db.insert(
        &UsageRow::new(now - 8, 3, Some("k-b"), Some("codex"), Some("m2"), 200)
            .with_tokens(7, 8, 0, 0),
    )
    .unwrap();

    let report = db
        .query(&UsageQuery::new(UsageWindow::All, now))
        .expect("query");
    let a = report
        .tenants
        .iter()
        .find(|t| t.tenant == "k-a")
        .expect("k-a present");
    assert_eq!((a.requests, a.ok, a.errors), (2, 1, 1));
    assert_eq!((a.tokens_in, a.tokens_out), (11, 22));
    assert_eq!((a.first_ms, a.last_ms), (now - 10, now - 9), "span");
    let cell = &a.models[0];
    assert_eq!(cell.group, "claude");
    assert_eq!((cell.cache_read, cell.cache_creation), (5, 1));
    assert_eq!(
        report.available_models,
        vec!["m1".to_string(), "m2".to_string()],
        "every observed model of the window, sorted"
    );
}

/// K-3: the observed-model list follows the WINDOW (the picker offers what the
/// selected period actually saw), not the whole file.
#[test]
fn available_models_follow_the_window() {
    let now = 2_000_000_000_000_u64;
    let db = UsageDb::open_in_memory().expect("open");
    db.insert(&row(now - 2 * 3_600_000, 1, "k-a", Some("old-model")))
        .unwrap();
    db.insert(&row(now - 60_000, 2, "k-a", Some("fresh-model")))
        .unwrap();

    let report = db
        .query(&UsageQuery::new(UsageWindow::H1, now))
        .expect("query");
    assert_eq!(report.available_models, vec!["fresh-model".to_string()]);
}

/// K-3: the offered model list is a WINDOW-only distinct query, never the
/// filtered aggregate. Selecting A used to make B and C vanish from the
/// picker, so a filter could only ever be narrowed, never changed.
#[test]
fn available_models_ignore_the_active_model_filter() {
    let now = 2_000_000_000_000_u64;
    let db = UsageDb::open_in_memory().expect("open");
    db.insert(&row(now - 10, 1, "k-a", Some("model-a")))
        .unwrap();
    db.insert(&row(now - 9, 2, "k-a", Some("model-b"))).unwrap();
    db.insert(&row(now - 8, 3, "k-a", Some("model-c"))).unwrap();

    let filtered = db
        .query(&UsageQuery::new(UsageWindow::All, now).with_models(["model-a"]))
        .expect("query");
    assert_eq!(filtered.rows, 1, "the ROWS honor the filter");
    assert_eq!(
        filtered.available_models,
        vec![
            "model-a".to_string(),
            "model-b".to_string(),
            "model-c".to_string()
        ],
        "every model of the window stays selectable"
    );
}

/// …and the offered list still follows the WINDOW, filter or not.
#[test]
fn available_models_follow_the_window_under_a_filter() {
    let now = 2_000_000_000_000_u64;
    let db = UsageDb::open_in_memory().expect("open");
    db.insert(&row(now - 2 * 3_600_000, 1, "k-a", Some("old-model")))
        .unwrap();
    db.insert(&row(now - 60_000, 2, "k-a", Some("fresh-model")))
        .unwrap();

    let report = db
        .query(&UsageQuery::new(UsageWindow::H1, now).with_models(["fresh-model"]))
        .expect("query");
    assert_eq!(report.available_models, vec!["fresh-model".to_string()]);
}

/// K-3: accounting models are normalized exactly like the in-memory fold, so a
/// `…[1m]` context hint never splits a model into two filter entries.
#[test]
fn recorded_models_are_normalized_like_the_activity_fold() {
    let now = 2_000_000_000_000_u64;
    let db = UsageDb::open_in_memory().expect("open");
    db.insert(&row(now - 10, 1, "k-a", Some("claude-sonnet-4-5[1m]")))
        .unwrap();
    db.insert(&row(now - 9, 2, "k-a", Some("claude-sonnet-4-5")))
        .unwrap();

    let report = db
        .query(&UsageQuery::new(UsageWindow::All, now))
        .expect("query");
    assert_eq!(
        report.available_models,
        vec!["claude-sonnet-4-5".to_string()],
        "one model, not two"
    );
    assert_eq!(report.tenants[0].models[0].requests, 2);
}

// ---------------------------------------------------------------------------
// 3. Legacy JSONL import — exactly once, restart-stable, overlap-safe
// ---------------------------------------------------------------------------

/// K-4: importing the same prefix twice imports it ONCE (persistent byte
/// offset + row identity), and a fresh process over the same file adds nothing.
#[test]
fn legacy_import_runs_exactly_once_across_restarts() {
    let tmp = TempDir::new();
    let jsonl = tmp.path().join("activity.jsonl");
    let dbp = tmp.path().join("usage.sqlite3");
    for i in 0..3 {
        append(&jsonl, &jsonl_line(1_000 + i, i, "k-a", "m1", 200));
    }
    let cut = file_len(&jsonl);

    let first = {
        let mut db = UsageDb::open(&dbp).expect("open");
        let outcome = db.import_activity_jsonl(&jsonl, cut).expect("import");
        assert_eq!(outcome.imported, 3, "first import takes the whole prefix");
        let again = db.import_activity_jsonl(&jsonl, cut).expect("import twice");
        assert_eq!(again.imported, 0, "second import is a no-op");
        db.row_count().expect("count")
    };
    assert_eq!(first, 3);

    // Restart: a brand-new connection over the same file.
    let mut db = UsageDb::open(&dbp).expect("reopen");
    let outcome = db.import_activity_jsonl(&jsonl, cut).expect("import");
    assert_eq!(outcome.imported, 0, "restart re-imports nothing");
    assert_eq!(db.row_count().expect("count"), 3, "counts are equal");
}

/// K-4: the import resumes at the persisted offset, so history appended
/// between boots is picked up exactly once.
#[test]
fn legacy_import_resumes_from_the_persisted_offset() {
    let tmp = TempDir::new();
    let jsonl = tmp.path().join("activity.jsonl");
    let dbp = tmp.path().join("usage.sqlite3");
    append(&jsonl, &jsonl_line(1_000, 1, "k-a", "m1", 200));
    let cut1 = file_len(&jsonl);
    let mut db = UsageDb::open(&dbp).expect("open");
    assert_eq!(db.import_activity_jsonl(&jsonl, cut1).unwrap().imported, 1);

    append(&jsonl, &jsonl_line(2_000, 2, "k-a", "m1", 200));
    append(&jsonl, &jsonl_line(3_000, 3, "k-b", "m2", 500));
    let cut2 = file_len(&jsonl);
    assert_eq!(
        db.import_activity_jsonl(&jsonl, cut2).unwrap().imported,
        2,
        "only the new bytes"
    );
    assert_eq!(db.row_count().unwrap(), 3);
}

/// K-4: the import cut + row identity together keep a request that was written
/// live (after the cut) from being counted a second time when the file is
/// imported again with a larger cut.
#[test]
fn live_rows_appended_after_the_cut_are_never_double_counted() {
    let tmp = TempDir::new();
    let jsonl = tmp.path().join("activity.jsonl");
    let dbp = tmp.path().join("usage.sqlite3");
    for i in 0..2 {
        append(&jsonl, &jsonl_line(1_000 + i, i, "k-a", "m1", 200));
    }
    let cut = file_len(&jsonl); // what `serve` records before live appends

    let mut db = UsageDb::open(&dbp).expect("open");
    // The live daemon records two more requests AND they also land in the
    // legacy log (both paths stay on, by contract).
    for i in 2..4 {
        db.insert(&row(1_000 + i, i, "k-a", Some("m1"))).unwrap();
        append(&jsonl, &jsonl_line(1_000 + i, i, "k-a", "m1", 200));
    }
    assert_eq!(db.import_activity_jsonl(&jsonl, cut).unwrap().imported, 2);
    assert_eq!(db.row_count().unwrap(), 4, "2 imported + 2 live");

    // A later import over the WHOLE file re-reads the overlapping range and
    // must still not duplicate the live rows.
    let outcome = db
        .import_activity_jsonl(&jsonl, file_len(&jsonl))
        .expect("import rest");
    assert_eq!(outcome.duplicates, 2, "the live rows are recognized");
    assert_eq!(db.row_count().unwrap(), 4, "nothing double-counted");
}

/// K-4: a half-written trailing line is left for the next import instead of
/// being consumed (the offset only advances over complete records).
#[test]
fn legacy_import_leaves_a_partial_trailing_line_for_the_next_pass() {
    let tmp = TempDir::new();
    let jsonl = tmp.path().join("activity.jsonl");
    let dbp = tmp.path().join("usage.sqlite3");
    append(&jsonl, &jsonl_line(1_000, 1, "k-a", "m1", 200));
    let complete = file_len(&jsonl);
    append(&jsonl, "{\"v\":1,\"ts_ms\":2000,\"id\":2,\"met");
    let torn = file_len(&jsonl);

    let mut db = UsageDb::open(&dbp).expect("open");
    assert_eq!(db.import_activity_jsonl(&jsonl, torn).unwrap().imported, 1);
    assert_eq!(
        db.import_offset(&jsonl).unwrap(),
        complete,
        "offset stops at the last complete line"
    );

    // The rest of that line arrives; the next import completes it.
    append(&jsonl, "hod\":\"POST\",\"path\":\"/v1/messages\",\"account\":null,\"status\":200,\"duration_ms\":1,\"tokens\":null,\"group\":\"claude\",\"model\":\"m1\",\"effort\":null,\"tenant\":\"k-a\"}\n");
    assert_eq!(
        db.import_activity_jsonl(&jsonl, file_len(&jsonl))
            .unwrap()
            .imported,
        1,
        "the completed line imports on the next pass"
    );
    assert_eq!(db.row_count().unwrap(), 2);
}

/// K-4: a truncated/rotated source (offset past the end) restarts from zero
/// rather than skipping the file forever; identity keeps counts correct.
#[test]
fn legacy_import_recovers_from_a_rotated_source() {
    let tmp = TempDir::new();
    let jsonl = tmp.path().join("activity.jsonl");
    let dbp = tmp.path().join("usage.sqlite3");
    append(&jsonl, &jsonl_line(1_000, 1, "k-a", "m1", 200));
    append(&jsonl, &jsonl_line(1_001, 2, "k-a", "m1", 200));
    let mut db = UsageDb::open(&dbp).expect("open");
    db.import_activity_jsonl(&jsonl, file_len(&jsonl)).unwrap();

    // Rotation: the file is replaced by a shorter one with NEW requests.
    std::fs::remove_file(&jsonl).expect("rotate");
    append(&jsonl, &jsonl_line(9_000, 9, "k-c", "m3", 200));
    let outcome = db
        .import_activity_jsonl(&jsonl, file_len(&jsonl))
        .expect("import after rotation");
    assert_eq!(outcome.imported, 1, "the new file is read from its start");
    assert_eq!(db.row_count().unwrap(), 3);
}

/// K-4: legacy lines with no tenant replay into `unknown` — never coerced into
/// a live bucket — and their models are still normalized.
#[test]
fn legacy_lines_without_a_tenant_import_as_unknown() {
    let tmp = TempDir::new();
    let jsonl = tmp.path().join("activity.jsonl");
    let dbp = tmp.path().join("usage.sqlite3");
    append(
        &jsonl,
        &(serde_json::json!({
            "v": 1, "ts_ms": 5_000, "id": 1, "method": "POST", "path": "/v1/messages",
            "account": "a", "status": 200, "duration_ms": 3,
            "tokens": { "input": 4, "output": 5, "cache_read": null, "cache_creation": null },
            "group": "claude", "model": "claude-sonnet-4-5[1m]"
        })
        .to_string()
            + "\n"),
    );
    let mut db = UsageDb::open(&dbp).expect("open");
    db.import_activity_jsonl(&jsonl, file_len(&jsonl)).unwrap();

    let report = db
        .query(&UsageQuery::new(UsageWindow::All, 10_000))
        .expect("query");
    assert_eq!(report.tenants[0].tenant, "unknown");
    assert_eq!(
        report.available_models,
        vec!["claude-sonnet-4-5".to_string()]
    );
}

/// K-5: only a MISSING source is benign. Any other stat failure (a path whose
/// parent is a file, a permission problem) propagates — an unreadable history
/// must not look like an empty one.
#[test]
fn an_unreadable_source_is_an_error_not_an_empty_import() {
    let tmp = TempDir::new();
    let blocker = tmp.path().join("not-a-dir");
    std::fs::write(&blocker, b"x").expect("write");
    let jsonl = blocker.join("activity.jsonl"); // parent is a FILE
    let mut db = UsageDb::open(&tmp.path().join("usage.sqlite3")).expect("open");

    match db.import_activity_jsonl(&jsonl, 1_000) {
        Ok(outcome) => panic!("an unreadable source must not report success: {outcome:?}"),
        Err(err) => assert!(
            err.to_string().contains("usage store io"),
            "the error names the failing io: {err}"
        ),
    }

    // A genuinely absent file stays a silent no-op (first boot).
    let missing = tmp.path().join("never-written.jsonl");
    assert_eq!(
        db.import_activity_jsonl(&missing, 1_000).expect("no-op"),
        llmux::key_usage::ImportOutcome::default()
    );
}

/// K-4: a REPLACED source of the same (or greater) length is a new file, not
/// more of the old one. The offset alone cannot see that, so the store also
/// remembers which file it read; a different one restarts from zero and the
/// row identities keep the counts exact.
#[test]
fn a_same_length_replacement_is_re_read_from_the_start() {
    let tmp = TempDir::new();
    let jsonl = tmp.path().join("activity.jsonl");
    let mut db = UsageDb::open(&tmp.path().join("usage.sqlite3")).expect("open");
    // Two-digit ids and four-digit stamps on both sides, so the replacement
    // file lands at exactly the same byte length as the original.
    for i in 0..2 {
        append(&jsonl, &jsonl_line(1_000 + i, 10 + i, "k-a", "m1", 200));
    }
    let first_len = file_len(&jsonl);
    assert_eq!(
        db.import_activity_jsonl(&jsonl, first_len)
            .unwrap()
            .imported,
        2
    );

    // Rotation by replacement: a brand-new file of the SAME byte length with
    // DIFFERENT requests (ids/timestamps padded to keep the length equal).
    std::fs::remove_file(&jsonl).expect("rotate");
    for i in 0..2 {
        append(&jsonl, &jsonl_line(9_000 + i, 90 + i, "k-c", "m1", 200));
    }
    assert_eq!(
        file_len(&jsonl),
        first_len,
        "the replacement is the same size"
    );

    let outcome = db
        .import_activity_jsonl(&jsonl, file_len(&jsonl))
        .expect("import after replacement");
    assert_eq!(outcome.imported, 2, "the new file is read from its start");
    assert_eq!(db.row_count().unwrap(), 4);
}

/// K-4: `copytruncate`-style rotation — same file, truncated and refilled —
/// also restarts, and re-reading an overlapping range never double-counts.
#[test]
fn a_truncated_and_refilled_source_restarts_without_double_counting() {
    let tmp = TempDir::new();
    let jsonl = tmp.path().join("activity.jsonl");
    let mut db = UsageDb::open(&tmp.path().join("usage.sqlite3")).expect("open");
    for i in 0..4 {
        append(&jsonl, &jsonl_line(1_000 + i, i, "k-a", "m1", 200));
    }
    db.import_activity_jsonl(&jsonl, file_len(&jsonl)).unwrap();
    assert_eq!(db.row_count().unwrap(), 4);

    // Truncate in place, then refill with one OLD record and one NEW one.
    std::fs::write(&jsonl, b"").expect("truncate");
    append(&jsonl, &jsonl_line(1_000, 0, "k-a", "m1", 200)); // already stored
    append(&jsonl, &jsonl_line(5_000, 5, "k-b", "m2", 200)); // new
    let outcome = db
        .import_activity_jsonl(&jsonl, file_len(&jsonl))
        .expect("import after truncate");
    assert_eq!(outcome.imported, 1, "only the new record is added");
    assert_eq!(outcome.duplicates, 1, "the re-read record is recognized");
    assert_eq!(db.row_count().unwrap(), 5);
}

/// K-4: lines the importer cannot use are COUNTED, so an import that stored
/// nothing can still be reported instead of passing in silence.
#[test]
fn unusable_lines_are_counted_as_skipped() {
    let tmp = TempDir::new();
    let jsonl = tmp.path().join("activity.jsonl");
    append(&jsonl, "{not json}\n");
    append(
        &jsonl,
        &(serde_json::json!({ "v": 99, "ts_ms": 1, "id": 1, "status": 200 }).to_string() + "\n"),
    );
    let mut db = UsageDb::open(&tmp.path().join("usage.sqlite3")).expect("open");
    let outcome = db
        .import_activity_jsonl(&jsonl, file_len(&jsonl))
        .expect("import");
    assert_eq!(outcome.imported, 0);
    assert_eq!(outcome.skipped, 2, "unparseable + unknown-version lines");
    assert!(outcome.bytes > 0, "the offset still advanced past them");
}

// ---------------------------------------------------------------------------
// 4. Store lifecycle: permissions, failure visibility, async writes
// ---------------------------------------------------------------------------

/// K-5: a long import must not freeze the daemon. The store parses OUTSIDE
/// its lock and commits one chunk at a time, so live writes and admin queries
/// interleave with an unfinished import instead of queueing behind it.
///
/// The proof is an observation of a PARTIAL state: while the import runs, a
/// query returns a row count strictly between empty and complete. With the
/// lock held for the whole import every sample would be 0 (blocked) or the
/// final total (after) — which is exactly how this test fails if the chunking
/// regresses.
#[test]
fn queries_and_live_writes_interleave_with_an_unfinished_import() {
    const LINES: u64 = 30_000;
    let tmp = TempDir::new();
    let jsonl = tmp.path().join("activity.jsonl");
    {
        use std::io::Write as _;
        let mut f = std::io::BufWriter::new(std::fs::File::create(&jsonl).expect("create"));
        for i in 0..LINES {
            f.write_all(jsonl_line(1_000 + i, i, "k-a", "m1", 200).as_bytes())
                .expect("write");
        }
    }
    let store = KeyUsageStore::open(&tmp.path().join("usage.sqlite3")).expect("open store");
    let cut = file_len(&jsonl);

    let importer = {
        let store = store.clone();
        let jsonl = jsonl.clone();
        std::thread::spawn(move || store.import_activity_jsonl(&jsonl, cut).expect("import"))
    };

    // Sample the store while the import runs. `now_ms` is far in the future so
    // every imported row is inside the window.
    let now = 9_000_000_000_000_u64;
    let mut saw_partial = false;
    while !importer.is_finished() {
        let report = store
            .query(&UsageQuery::new(UsageWindow::All, now))
            .expect("query answered during the import");
        if report.rows > 0 && report.rows < LINES {
            saw_partial = true;
        }
        // A live request lands mid-import too.
        store.record(row(8_000_000_000_000, 1, "k-live", Some("m-live")));
    }
    let outcome = importer.join().expect("import thread");
    assert_eq!(outcome.imported, LINES, "every legacy row imported once");
    assert!(
        saw_partial,
        "a query answered mid-import (the lock is released between chunks)"
    );

    wait_for(
        || store.health().written > 0,
        "the live rows written during the import",
    );
    let final_report = store
        .query(&UsageQuery::new(UsageWindow::All, now))
        .expect("query");
    assert_eq!(
        final_report.rows,
        LINES + 1,
        "imported rows + exactly one live row (identical live rows dedupe)"
    );
}

/// K-5: a failed migration leaves PARTIAL history in the store. The failure is
/// recorded in the health block — not only in a log line — so every later
/// answer carries the evidence that it may be incomplete.
#[test]
fn a_failed_import_is_recorded_in_the_store_health() {
    let tmp = TempDir::new();
    let blocker = tmp.path().join("not-a-dir");
    std::fs::write(&blocker, b"x").expect("write");
    let store = KeyUsageStore::open(&tmp.path().join("usage.sqlite3")).expect("open store");
    assert_eq!(store.health().errors, 0);

    let err = store
        .import_activity_jsonl(&blocker.join("activity.jsonl"), 1_000)
        .expect_err("unreadable source");
    let health = store.health();
    assert_eq!(health.errors, 1, "the failure is counted");
    assert_eq!(
        health.last_error.as_deref(),
        Some(err.to_string().as_str()),
        "and named"
    );
    assert!(!health.importing, "the import is no longer running");
}

/// K-4: the DB is private to its owner (it carries per-tenant metering, and
/// sits next to the config that holds credentials).
#[cfg(unix)]
#[test]
fn the_database_file_and_directory_are_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = TempDir::new();
    let dbp = tmp.path().join("nested").join("usage.sqlite3");
    let db = UsageDb::open(&dbp).expect("open");
    db.insert(&row(1, 1, "k-a", Some("m1"))).unwrap();
    drop(db);

    let file_mode = std::fs::metadata(&dbp).unwrap().permissions().mode() & 0o777;
    assert_eq!(file_mode, 0o600, "db file is rw for the owner only");
    let dir_mode = std::fs::metadata(dbp.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "db directory is owner-only");
}

/// K-5: an unusable location is an ERROR, never a silently empty store — a
/// failed open must not render as a valid zero.
#[test]
fn opening_an_unusable_path_reports_an_error() {
    let tmp = TempDir::new();
    let dir = tmp.path().join("not-a-file");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let err = match UsageDb::open(&dir) {
        Ok(_) => panic!("a directory is not a database"),
        Err(err) => err,
    };
    assert!(
        err.to_string().to_lowercase().contains("usage"),
        "the error names the usage store: {err}"
    );
}

/// K-5/K-6: the async store records off the caller's thread and its health
/// counters are the receipt that the rows actually landed.
#[test]
fn the_store_records_asynchronously_and_reports_written_rows() {
    let tmp = TempDir::new();
    let dbp = tmp.path().join("usage.sqlite3");
    let store = KeyUsageStore::open(&dbp).expect("open store");
    let now = 2_000_000_000_000_u64;
    for i in 0..5 {
        store.record(row(now - i, i, "k-a", Some("m1")));
    }
    wait_for(|| store.health().written >= 5, "5 rows written");

    let report = store
        .query(&UsageQuery::new(UsageWindow::All, now))
        .expect("query");
    assert_eq!(report.rows, 5);
    assert_eq!(store.health().errors, 0, "no write errors");
    assert_eq!(store.health().dropped, 0, "nothing dropped");
    assert_eq!(
        store.health().path.as_deref(),
        Some(dbp.to_string_lossy().as_ref()),
        "health names the file it writes"
    );
}

/// K-4: the channel-derived location — default config path → the channel
/// directory; an explicit `$LLMUX_CONFIG` → an isolated sibling directory
/// named after the config file (so a test/alternate config never shares the
/// user's DB).
#[test]
fn the_database_location_is_derived_from_the_config_path() {
    let default_cfg = Path::new("/home/u/.config/llmux.json");
    assert_eq!(
        llmux::key_usage::db_path_for(Some(default_cfg), Some(default_cfg), "stable"),
        Some(PathBuf::from("/home/u/.config/llmux/usage.sqlite3"))
    );
    assert_eq!(
        llmux::key_usage::db_path_for(Some(default_cfg), Some(default_cfg), "preview"),
        Some(PathBuf::from("/home/u/.config/llmux-preview/usage.sqlite3")),
        "the preview channel keeps its own history"
    );
    let explicit = Path::new("/tmp/t123/alt.json");
    assert_eq!(
        llmux::key_usage::db_path_for(Some(explicit), Some(default_cfg), "stable"),
        Some(PathBuf::from("/tmp/t123/alt/usage.sqlite3")),
        "an explicit config override gets an isolated sibling directory"
    );
    assert_eq!(
        llmux::key_usage::db_path_for(None, Some(default_cfg), "stable"),
        None,
        "no config persistence → no durable usage store"
    );
}

// ---------------------------------------------------------------------------
// 5. Daemon acceptance: GET /llmux/keys/usage
// ---------------------------------------------------------------------------

struct Proxy {
    addr: SocketAddr,
    events: tokio::sync::mpsc::Sender<ActivityEvent>,
}

impl Proxy {
    /// Spawn a daemon whose config (and therefore usage DB) lives in `tmp`.
    /// `seed` runs before startup so a test can plant a legacy activity log.
    async fn spawn_in(dir: &Path, seed: impl FnOnce(&Path)) -> Self {
        let mut config = Config {
            client_keys: vec![ClientKey {
                id: "k-alpha".into(),
                name: "alpha-pc".into(),
                email: Some("a@example.com".into()),
                kind: ClientKeyKind::Default,
                key_prefix: "lmk-aaaa".into(),
                key_digest: format!("sha256:{}", "0".repeat(64)),
                suspended: false,
                created_at_ms: 1,
                revoked_at_ms: None,
            }],
            ..Default::default()
        };
        config.proxy.idle_probe.enabled = false;
        config.proxy.api_key = Some(ADMIN_KEY.into());
        config.proxy.port = 0;
        let config_path = dir.join("llmux.json");
        config::save_path(&config_path, &config).expect("seed config");
        seed(dir);

        let pool = AccountPool::new(&config.accounts);
        let mut state = AppState::new(config, pool, None, None).expect("app state");
        state.config_path = Some(config_path);
        state.activity_log_path = Some(dir.join("activity.jsonl"));
        state.raw_io_path = Some(dir.join("raw-io.jsonl"));
        state.usage_control_state_path = Some(dir.join("usage-resets.json"));
        let events = state.events.clone().expect("event sender");

        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(serve(state, Some(ready_tx)));
        let addr = ready_rx.await.expect("proxy ready");
        Self { addr, events }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.addr.port(), path)
    }

    /// Drive one finished request through the daemon's real event fold.
    async fn finish(&self, id: u64, tenant: &str, model: &str, status: u16) {
        self.events
            .send(ActivityEvent::RequestFinished {
                id,
                method: "POST".into(),
                path: "/v1/messages".into(),
                account: Some("acct".into()),
                status,
                duration: Duration::from_millis(5),
                tokens: Some(TokenCounts {
                    input: 100,
                    output: 200,
                    cache_read: Some(10),
                    cache_creation: Some(1),
                }),
                group: Some("claude".into()),
                model: Some(model.into()),
                effort: None,
                fast: Some(false),
                ttfb_ms: None,
                ttft_ms: None,
                gen_ms: None,
                aborted: false,
                user_id: None,
                kind: None,
                excerpt: None,
                tenant: Some(tenant.into()),
            })
            .await
            .expect("event accepted");
    }
}

async fn get_usage(proxy: &Proxy, query: &str, key: Option<&str>) -> (u16, serde_json::Value) {
    let client = reqwest::Client::new();
    let mut request = client.get(proxy.url(&format!("/llmux/keys/usage{query}")));
    if let Some(key) = key {
        request = request.header("x-api-key", key);
    }
    let response = request.send().await.expect("reachable");
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    (
        status,
        serde_json::from_str(&body).unwrap_or(serde_json::Value::String(body)),
    )
}

/// Poll until `ready`, failing the test after 5s rather than hanging.
fn wait_for(mut ready: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

/// Poll an async condition until it holds (the fold + writer thread are
/// asynchronous by design).
async fn wait_for_usage(
    proxy: &Proxy,
    query: &str,
    mut ready: impl FnMut(&serde_json::Value) -> bool,
    what: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = serde_json::Value::Null;
    while Instant::now() < deadline {
        let (status, body) = get_usage(proxy, query, Some(ADMIN_KEY)).await;
        assert_eq!(status, 200, "usage query failed: {body}");
        if ready(&body) {
            return body;
        }
        last = body;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}; last body: {last}");
}

/// K-1/K-6: a finished request becomes a durable, queryable tenant row with
/// the key's display name attached and its per-model cell priced.
#[tokio::test]
async fn finished_requests_become_queryable_tenant_rows() {
    let tmp = TempDir::new();
    let proxy = Proxy::spawn_in(tmp.path(), |_| {}).await;
    proxy
        .finish(1, "k-alpha", "claude-sonnet-4-5[1m]", 200)
        .await;
    proxy.finish(2, "k-alpha", "claude-sonnet-4-5", 429).await;

    let body = wait_for_usage(
        &proxy,
        "",
        |b| b["tenants"][0]["requests"].as_u64() == Some(2),
        "both requests recorded",
    )
    .await;
    assert_eq!(body["window"], "all");
    let tenant = &body["tenants"][0];
    assert_eq!(tenant["tenant"], "k-alpha");
    assert_eq!(
        tenant["name"], "alpha-pc",
        "display name joined from the key"
    );
    assert_eq!(tenant["ok"], 1);
    assert_eq!(tenant["errors"], 1);
    assert_eq!(tenant["tokens_in"], 200);
    assert_eq!(
        body["available_models"],
        serde_json::json!(["claude-sonnet-4-5"]),
        "normalized once, offered once"
    );
    assert!(
        tenant["models"][0]["cost_usd"].as_f64().unwrap() > 0.0,
        "the model cell is priced server-side: {tenant}"
    );
    assert!(
        !body.to_string().contains("llmuxk_"),
        "no key material on the wire: {body}"
    );
}

/// K-2: an unknown window is a client error, not a silent fallback to `all`.
#[tokio::test]
async fn an_unknown_window_is_rejected_with_400() {
    let tmp = TempDir::new();
    let proxy = Proxy::spawn_in(tmp.path(), |_| {}).await;
    let (status, body) = get_usage(&proxy, "?window=2h", Some(ADMIN_KEY)).await;
    assert_eq!(status, 400, "body: {body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("window"),
        "the error names the bad parameter: {body}"
    );
}

/// K-1: the usage surface sits behind the control-plane gate like the rest of
/// `/llmux/*` — a non-admin credential cannot read cross-tenant metering.
/// (A loopback peer with an unknown key resolves to the non-admin `local`
/// tenant, so the refusal is 403, not 401.)
#[tokio::test]
async fn the_usage_endpoint_requires_an_admin_credential() {
    let tmp = TempDir::new();
    let proxy = Proxy::spawn_in(tmp.path(), |_| {}).await;
    let (status, body) = get_usage(&proxy, "", Some("not-the-admin-key")).await;
    assert_eq!(
        status, 403,
        "a non-admin cannot read tenant metering: {body}"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("admin"),
        "the refusal names the missing scope: {body}"
    );
}

/// K-3: the endpoint's window + model filters produce the same intersection
/// the store does, end to end.
#[tokio::test]
async fn the_endpoint_filters_by_window_and_model() {
    let tmp = TempDir::new();
    let proxy = Proxy::spawn_in(tmp.path(), |_| {}).await;
    proxy.finish(1, "k-alpha", "model-a", 200).await;
    proxy.finish(2, "k-alpha", "model-b", 200).await;
    wait_for_usage(
        &proxy,
        "",
        |b| b["tenants"][0]["requests"].as_u64() == Some(2),
        "both models recorded",
    )
    .await;

    let (status, body) = get_usage(&proxy, "?window=24h&models=model-a", Some(ADMIN_KEY)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["window"], "24h");
    assert_eq!(body["models"], serde_json::json!(["model-a"]));
    assert_eq!(body["tenants"][0]["requests"], 1, "{body}");
    assert_eq!(body["tenants"][0]["models"][0]["model"], "model-a");
}

/// K-4: a legacy `activity.jsonl` is imported into the durable store once, and
/// a RESTART over the same directory neither re-imports it nor loses it.
#[tokio::test]
async fn a_legacy_activity_log_is_imported_once_and_survives_restart() {
    let tmp = TempDir::new();
    let proxy = Proxy::spawn_in(tmp.path(), |dir| {
        let jsonl = dir.join("activity.jsonl");
        for i in 0..4 {
            append(
                &jsonl,
                &jsonl_line(1_700_000_000_000 + i, i, "k-alpha", "legacy-model", 200),
            );
        }
    })
    .await;
    let body = wait_for_usage(
        &proxy,
        "",
        |b| b["tenants"][0]["requests"].as_u64() == Some(4),
        "legacy history imported",
    )
    .await;
    assert_eq!(
        body["available_models"],
        serde_json::json!(["legacy-model"])
    );
    drop(proxy);

    // Restart over the SAME directory (config + activity log + usage DB).
    let restarted = Proxy::spawn_in(tmp.path(), |_| {}).await;
    let body = wait_for_usage(
        &restarted,
        "",
        |b| b["tenants"][0]["requests"].as_u64() == Some(4),
        "history still there after restart",
    )
    .await;
    assert_eq!(
        body["tenants"][0]["requests"], 4,
        "restart neither duplicates nor drops: {body}"
    );
}
