-- PostgreSQL (>= 14) schema for the llmux and soma-work stores.
--
-- Every table has the same rows and columns as its SQLite source, so a move
-- to a server database is a copy, not a conversion. Loader and verifier:
-- scripts/dbms/sqlite_to_pg.py. Procedure: docs/dbms-migration.md.

-- raw-io: one SQLite file per local day (store/raw-io/YYYY-MM-DD.sqlite3)
-- becomes one partition of a range-partitioned table. The loader creates the
-- partition for each day it loads.
CREATE TABLE IF NOT EXISTS raw_io (
    day         date    NOT NULL,   -- the partition file's local day
    seq         bigint  NOT NULL,   -- insertion order within the day
    ts_ms       bigint  NOT NULL,
    id          bigint  NOT NULL,
    grp         text,
    model       text,
    account     text,
    status      integer,
    duration_ms bigint,
    user_id     text,
    tokens_in   bigint  NOT NULL DEFAULT 0,
    tokens_out  bigint  NOT NULL DEFAULT 0,
    parsed      boolean NOT NULL,
    rec_hash    bytea   NOT NULL,   -- sha256(line)
    src         text,               -- import provenance (NULL = live write)
    src_off     bigint,
    line        bytea   NOT NULL,   -- the exact original JSON line
    PRIMARY KEY (day, seq)
) PARTITION BY RANGE (day);
CREATE INDEX IF NOT EXISTS raw_io_id_ts ON raw_io (id, ts_ms);
CREATE INDEX IF NOT EXISTS raw_io_ts ON raw_io (ts_ms);
-- SQLite: UNIQUE (src, src_off). A partitioned unique index must include
-- the partition key; an import offset belongs to exactly one day.
CREATE UNIQUE INDEX IF NOT EXISTS raw_io_src ON raw_io (day, src, src_off);

-- llmux keys-usage statistics (usage.sqlite3).
CREATE TABLE IF NOT EXISTS usage (
    event_key      text    PRIMARY KEY,
    ts_ms          bigint  NOT NULL,
    tenant         text    NOT NULL,
    grp            text,
    model          text,
    status         integer NOT NULL,
    tokens_in      bigint  NOT NULL DEFAULT 0,
    tokens_out     bigint  NOT NULL DEFAULT 0,
    cache_read     bigint  NOT NULL DEFAULT 0,
    cache_creation bigint  NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS usage_ts_idx ON usage (ts_ms);
CREATE INDEX IF NOT EXISTS usage_model_ts_idx ON usage (model, ts_ms);

-- soma-work metrics ({DATA_DIR}/metrics/metrics.sqlite3). `json` stays text,
-- byte-exact (jsonb would normalise it); cast in queries when needed.
CREATE TABLE IF NOT EXISTS metrics_events (
    seq        bigint PRIMARY KEY,
    id         text   NOT NULL UNIQUE,
    ts         bigint NOT NULL,
    day        date   NOT NULL,
    event_type text,
    json       text   NOT NULL
);
CREATE INDEX IF NOT EXISTS metrics_events_day_ts ON metrics_events (day, ts);
