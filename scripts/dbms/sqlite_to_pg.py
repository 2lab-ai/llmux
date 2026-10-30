#!/usr/bin/env python3
"""Copy an llmux / soma-work SQLite store into PostgreSQL, and prove the copy.

    sqlite_to_pg.py load   KIND FILE            > psql script on stdout
    sqlite_to_pg.py verify KIND FILE --psql CMD [--manifest M.json]

KIND
    raw-io   one raw-io day partition, store/raw-io/YYYY-MM-DD.sqlite3
             (an archived day: `llmux raw-io restore DAY` first, or restore
             into a scratch store with `--store DIR`)
    usage    llmux keys-usage statistics, usage.sqlite3
    metrics  soma-work metrics, {DATA_DIR}/metrics/metrics.sqlite3

`load` writes one transaction (BEGIN; COPY ... FROM STDIN; COMMIT) for psql,
so a failed load leaves nothing behind:

    sqlite_to_pg.py load usage usage.sqlite3 | psql -v ON_ERROR_STOP=1 ...

`verify` streams the same rows back out of PostgreSQL (COPY ... TO STDOUT,
same order) and compares row count and a sha256 over every column of every
row with the SQLite file. For raw-io, `--manifest` also checks the day's row
count and line sha256 against the archive manifest. Exit status 0 = equal.

Read a live database from a snapshot (sqlite3 FILE ".backup COPY") so both
sides see the same rows. Standard library only.
"""

import argparse
import datetime
import hashlib
import json
import os
import shlex
import sqlite3
import subprocess
import sys

KINDS = {
    "raw-io": {
        "table": "raw_io",
        "source": "raw_io",
        "order": "seq",
        "cols": [
            ("day", "date"), ("seq", "int"), ("ts_ms", "int"), ("id", "int"),
            ("grp", "text"), ("model", "text"), ("account", "text"),
            ("status", "int"), ("duration_ms", "int"), ("user_id", "text"),
            ("tokens_in", "int"), ("tokens_out", "int"), ("parsed", "bool"),
            ("rec_hash", "bytea"), ("src", "text"), ("src_off", "int"),
            ("line", "bytea"),
        ],
    },
    "usage": {
        "table": "usage",
        "source": "usage",
        "order": "event_key",
        "cols": [
            ("event_key", "text"), ("ts_ms", "int"), ("tenant", "text"),
            ("grp", "text"), ("model", "text"), ("status", "int"),
            ("tokens_in", "int"), ("tokens_out", "int"),
            ("cache_read", "int"), ("cache_creation", "int"),
        ],
    },
    "metrics": {
        "table": "metrics_events",
        "source": "metrics_events",
        "order": "seq",
        "cols": [
            ("seq", "int"), ("id", "text"), ("ts", "int"), ("day", "date"),
            ("event_type", "text"), ("json", "text"),
        ],
    },
}

# COPY text format escapes (the set PostgreSQL's COPY TO emits).
_ESC = {"\\": "\\\\", "\b": "\\b", "\f": "\\f", "\n": "\\n", "\r": "\\r", "\t": "\\t", "\v": "\\v"}
_ESC_TABLE = str.maketrans(_ESC)


def enc(value, typ):
    if value is None:
        return "\\N"
    if typ == "bytea":
        return "\\\\x" + bytes(value).hex()
    if typ == "bool":
        return "t" if value else "f"
    if typ == "int":
        return str(int(value))
    return str(value).translate(_ESC_TABLE)


def raw_io_day(path):
    day = os.path.basename(path)[:10]
    datetime.date.fromisoformat(day)  # raises on a non-day file name
    return day


def sqlite_rows(kind, path):
    """COPY-text lines of the SQLite rows, in verification order."""
    spec = KINDS[kind]
    con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        if kind == "raw-io":
            day = raw_io_day(path)
            cols = [c for c, _ in spec["cols"] if c != "day"]
            prefix = [day]
            types = [t for c, t in spec["cols"] if c != "day"]
        else:
            cols = [c for c, _ in spec["cols"]]
            prefix = []
            types = [t for _, t in spec["cols"]]
        sql = f"SELECT {', '.join(cols)} FROM {spec['source']} ORDER BY {spec['order']}"
        for row in con.execute(sql):
            yield "\t".join(prefix + [enc(v, t) for v, t in zip(row, types)]) + "\n"
    finally:
        con.close()


def cmd_load(args):
    spec = KINDS[args.kind]
    out = sys.stdout
    out.write("\\set ON_ERROR_STOP on\nBEGIN;\n")
    if args.kind == "raw-io":
        day = raw_io_day(args.file)
        nxt = (datetime.date.fromisoformat(day) + datetime.timedelta(days=1)).isoformat()
        part = "raw_io_" + day.replace("-", "_")
        out.write(
            f"CREATE TABLE {part} PARTITION OF raw_io FOR VALUES FROM ('{day}') TO ('{nxt}');\n"
        )
    out.write(f"COPY {spec['table']} ({', '.join(c for c, _ in spec['cols'])}) FROM STDIN;\n")
    n = 0
    for line in sqlite_rows(args.kind, args.file):
        out.write(line)
        n += 1
    out.write("\\.\nCOMMIT;\n")
    print(f"{args.kind}: {n} rows emitted", file=sys.stderr)


def pg_digest(kind, path, psql):
    spec = KINDS[kind]
    cols = ", ".join(f"{c}" for c, _ in spec["cols"])
    order = spec["order"] + (' COLLATE "C"' if spec["order"] == "event_key" else "")
    where = f" WHERE day = '{raw_io_day(path)}'" if kind == "raw-io" else ""
    query = f"COPY (SELECT {cols} FROM {spec['table']}{where} ORDER BY {order}) TO STDOUT"
    proc = subprocess.Popen(shlex.split(psql) + ["-X", "-q", "-v", "ON_ERROR_STOP=1", "-c", query],
                            stdout=subprocess.PIPE)
    h, n = hashlib.sha256(), 0
    for line in proc.stdout:
        h.update(line)
        n += 1
    if proc.wait() != 0:
        raise SystemExit(f"psql failed with status {proc.returncode}")
    return n, h.hexdigest()


def cmd_verify(args):
    h, n = hashlib.sha256(), 0
    for line in sqlite_rows(args.kind, args.file):
        h.update(line.encode("utf-8", "surrogateescape"))
        n += 1
    sqlite_side = (n, h.hexdigest())
    pg_side = pg_digest(args.kind, args.file, args.psql)
    result = {
        "kind": args.kind,
        "file": args.file,
        "sqlite_rows": sqlite_side[0],
        "pg_rows": pg_side[0],
        "rows_sha256_sqlite": sqlite_side[1],
        "rows_sha256_pg": pg_side[1],
        "equal": sqlite_side == pg_side,
    }
    if args.kind == "raw-io" and args.manifest:
        m = json.load(open(args.manifest))
        con = sqlite3.connect(f"file:{args.file}?mode=ro", uri=True)
        lh, ln = hashlib.sha256(), 0
        for (line,) in con.execute("SELECT line FROM raw_io ORDER BY seq"):
            lh.update(bytes(line))
            lh.update(b"\n")
            ln += 1
        con.close()
        result.update(manifest_rows=m["rows"], manifest_plain_sha256=m["plain_sha256"],
                      lines_sha256=lh.hexdigest(),
                      manifest_equal=(ln == m["rows"] and lh.hexdigest() == m["plain_sha256"]))
        result["equal"] = result["equal"] and result["manifest_equal"]
    print(json.dumps(result, indent=1))
    return 0 if result["equal"] else 1


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    for name in ("load", "verify"):
        s = sub.add_parser(name)
        s.add_argument("kind", choices=sorted(KINDS))
        s.add_argument("file")
        if name == "verify":
            s.add_argument("--psql", required=True, help='e.g. "docker exec -i llmux-pg psql -U llmux -d llmux"')
            s.add_argument("--manifest", help="raw-io: archive manifest of the same day")
    args = p.parse_args()
    if args.cmd == "load":
        cmd_load(args)
        return 0
    return cmd_verify(args)


if __name__ == "__main__":
    sys.exit(main())
