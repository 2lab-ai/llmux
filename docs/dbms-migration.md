# Moving the stores to a server database

Today the raw-io capture, the keys-usage statistics and the soma-work metrics
are all stored in SQLite files on the same machine as the daemon. A server
database (PostgreSQL in Docker) can take them over later without converting
anything. Each table there has the same rows and columns as its SQLite file,
so a copy can be checked row for row.

| Store | SQLite file | PostgreSQL table |
| --- | --- | --- |
| raw-io capture | `~/.local/state/llmux/store/raw-io/YYYY-MM-DD.sqlite3`, table `raw_io` | `raw_io`, range-partitioned by `day`; one partition per day file |
| raw-io past days | `store/raw-io/archive/YYYY-MM-DD[.N].jsonl.zst` + `.manifest.json` | stay as files (see below); restore a day to load it |
| keys-usage statistics | `usage.sqlite3` next to the config, table `usage` | `usage` |
| soma-work metrics | `{DATA_DIR}/metrics/metrics.sqlite3`, tables `metrics_events` and `metrics_unparsed` (malformed lines kept verbatim) | `metrics_events`, `metrics_unparsed` |

Bookkeeping tables (`meta`, `metrics_jsonl_imports`, `metrics_exports`,
`metrics_day_rev`) describe the SQLite files and their archives. They are not
copied, because a server backend keeps its own.

The schema is in [`scripts/dbms/postgres-schema.sql`](../scripts/dbms/postgres-schema.sql).
The loader and verifier is [`scripts/dbms/sqlite_to_pg.py`](../scripts/dbms/sqlite_to_pg.py)
(Python standard library only). A container definition is in
[`scripts/dbms/docker-compose.yml`](../scripts/dbms/docker-compose.yml).

## Why past days stay as zstd files

Claude Code resends the whole conversation on every turn, so most of a day's
raw-io is repeated text. Compressing a whole day as one long-window zstd
stream removes the repetition. Compressing each record on its own, which is
what a database does with large values, cannot see it. For example, on
2026-08-12 (800 records):

| Form | Size |
| --- | --- |
| Plain lines | 877 MB |
| SQLite day partition | 888 MB |
| PostgreSQL partition (default TOAST compression, indexes included) | 770 MB |
| Day archive, zstd level 3 with a 1 GiB window | 6.9 MB |

So the server database should hold the hot days and the metadata columns.
Archived days stay as files on disk or in an object store. Load a day into
the database only when it needs to be queried.

## Procedure

1. Start PostgreSQL. The schema is applied the first time it starts on an
   empty data directory:

   ```sh
   # from the repository root
   LLMUX_PG_PASSWORD=... LLMUX_PG_DATA=/srv/llmux-pg docker compose -f scripts/dbms/docker-compose.yml up -d
   PSQL="docker exec -i llmux-pg psql -U llmux -d llmux"
   ```

2. Take a snapshot of every live database, so that the load and the check
   see the same rows. A WAL reader never blocks the writer:

   ```sh
   sqlite3 ~/.config/llmux-preview/usage.sqlite3 ".backup /tmp/usage.sqlite3"
   sqlite3 "$DATA_DIR/metrics/metrics.sqlite3" ".backup /tmp/metrics.sqlite3"
   ```

   Do not read a raw-io day partition in place. The daemon may seal it and
   move it to `sealing/` and `trash/` between the load and the check. Copy
   it, or restore the day into a scratch store. This works for archived
   days and leaves the live store unchanged:

   ```sh
   mkdir -p /tmp/rs/archive && cp ~/.local/state/llmux/store/raw-io/archive/2026-08-12.* /tmp/rs/archive/
   llmux raw-io restore 2026-08-12 --store /tmp/rs
   ```

3. Load. Each run is a single transaction, so a failure leaves nothing
   behind. A raw-io day also creates its partition:

   ```sh
   python3 scripts/dbms/sqlite_to_pg.py load raw-io  /tmp/rs/2026-08-12.sqlite3 | $PSQL
   python3 scripts/dbms/sqlite_to_pg.py load usage   /tmp/usage.sqlite3          | $PSQL
   python3 scripts/dbms/sqlite_to_pg.py load metrics /tmp/metrics.sqlite3        | $PSQL
   ```

4. Verify. The script reads every row back out of PostgreSQL, in the same
   order, and compares the row count and a sha256 over every column with
   the SQLite file. For a raw-io day, `--manifest` also checks the result
   against the archive manifest. Use it only for a day with a single
   generation. A day with `.1`, `.2`, ... restores as the union of all its
   generations and will not match any one manifest.

   Exit status 0 means everything is equal and 1 means something differs.
   Exit status 2 means a SQLite value cannot be copied without changing it,
   for example a REAL in an INTEGER column, a boolean other than 0 or 1, or
   TEXT in a BLOB column. Both `load` and `verify` refuse such a value
   instead of normalising it:

   ```sh
   python3 scripts/dbms/sqlite_to_pg.py verify raw-io /tmp/rs/2026-08-12.sqlite3 --psql "$PSQL" \
       --manifest /tmp/rs/archive/2026-08-12.manifest.json
   python3 scripts/dbms/sqlite_to_pg.py verify usage   /tmp/usage.sqlite3   --psql "$PSQL"
   python3 scripts/dbms/sqlite_to_pg.py verify metrics /tmp/metrics.sqlite3 --psql "$PSQL"
   ```

Keep the SQLite files and archives until the server copy has been checked.
Nothing in this procedure deletes them.

## Trial run (fable-m5max, 2026-10-31, PostgreSQL 17.11)

I ran the procedure above from an empty data directory:

| Store | Rows | Load | Verify |
| --- | --- | --- | --- |
| raw-io 2026-08-12 (restored from its archive) | 800 | 55 s | rows sha256 equal; lines sha256 `7b114508…` = manifest |
| keys usage (snapshot) | 1,489,885 | 14 s | rows sha256 equal |
| soma-work metrics (snapshot) | 37,193 + 0 unparsed | 1 s | rows sha256 equal |

The strict encoder accepted every value in the three real stores.

Negative checks, each in a separate database:

- **Unparsed line:** a `metrics_unparsed` row was copied and verified. After deleting it in PostgreSQL, `verify` exited with 1.
- **Changed token count:** adding one token to one usage row in PostgreSQL made `verify` exit with 1. Undoing the change made it pass again.
- **REAL in an INTEGER column:** a value such as 1774463816599.5 in `metrics_events.ts` makes both `load` and `verify` exit with 2. This holds whether the bad row comes first or last. When it is the last of 37,193 rows, the streamed rows are rolled back and nothing is stored.

## Moving the daemon itself

The daemon reaches raw-io only through `RawStore` in `src/proxy/raw_store.rs`:

- `capture`
- `find`
- `load_hot_records`
- `for_each_session_rows`
- `import_file(s)`
- `archive_cycle`
- `restore_day`
- `status`

The usage statistics go only through `UsageDb` / `KeyUsageStore` in
`src/key_usage.rs`.

A PostgreSQL backend implements these same calls against the tables above,
and the archive format and manifests stay as they are. Until then, the
SQLite files remain the live store and this procedure is a copy.
