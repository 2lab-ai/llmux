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
             (metrics_events and metrics_unparsed)

`load` writes one transaction (BEGIN; COPY ... FROM STDIN; COMMIT) for psql,
so a failed load leaves nothing behind:

    sqlite_to_pg.py load usage usage.sqlite3 | psql -v ON_ERROR_STOP=1 ...

`verify` streams the same rows back out of PostgreSQL (COPY ... TO STDOUT,
same order) and compares row count and a sha256 over every column of every
row with the SQLite file. For raw-io, `--manifest` also checks the day's row
count and line sha256 against the archive manifest. Exit status 0 = equal,
1 = different, 2 = a SQLite value that cannot be copied losslessly (a REAL
in an INTEGER column, a boolean other than 0/1, TEXT in a BLOB column...):
both commands refuse it instead of normalising it.

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
import urllib.parse

RAW_IO = {
    "table": "raw_io",
    "order": ["seq"],
    "cols": [
        ("day", "date"), ("seq", "int"), ("ts_ms", "int"), ("id", "int"),
        ("grp", "text"), ("model", "text"), ("account", "text"),
        ("status", "int"), ("duration_ms", "int"), ("user_id", "text"),
        ("tokens_in", "int"), ("tokens_out", "int"), ("parsed", "bool"),
        ("rec_hash", "bytea"), ("src", "text"), ("src_off", "int"),
        ("line", "bytea"),
    ],
}
USAGE = {
    "table": "usage",
    "order": ["event_key"],
    "cols": [
        ("event_key", "text"), ("ts_ms", "int"), ("tenant", "text"),
        ("grp", "text"), ("model", "text"), ("status", "int"),
        ("tokens_in", "int"), ("tokens_out", "int"),
        ("cache_read", "int"), ("cache_creation", "int"),
    ],
}
METRICS_EVENTS = {
    "table": "metrics_events",
    "order": ["seq"],
    "cols": [
        ("seq", "int"), ("id", "text"), ("ts", "int"), ("day", "date"),
        ("event_type", "text"), ("json", "text"),
    ],
}
# Malformed JSONL lines soma-work keeps verbatim: data, so they move too.
METRICS_UNPARSED = {
    "table": "metrics_unparsed",
    "order": ["file", "line_no"],
    "cols": [("file", "text"), ("line_no", "int"), ("text", "text")],
}
# Data tables per kind. Bookkeeping tables (`meta`, `metrics_jsonl_imports`,
# `metrics_exports`, `metrics_day_rev`) describe the SQLite files and their
# archives; a server backend keeps its own.
KINDS = {
    "raw-io": [RAW_IO],
    "usage": [USAGE],
    "metrics": [METRICS_EVENTS, METRICS_UNPARSED],
}
TEXT_ORDER = {"event_key", "file"}  # ordered bytewise: COLLATE "C" in PostgreSQL

# COPY text format escapes (the set PostgreSQL's COPY TO emits).
_ESC = {"\\": "\\\\", "\b": "\\b", "\f": "\\f", "\n": "\\n", "\r": "\\r", "\t": "\\t", "\v": "\\v"}
_ESC_TABLE = str.maketrans(_ESC)


class Unsupported(ValueError):
    pass


def enc(value, typ, where):
    """COPY text for one SQLite value. Strict: a value that would change on
    the way into PostgreSQL (a REAL in an INTEGER column, 2 in a boolean,
    TEXT in a BLOB...) raises instead of being normalised, because the
    verifier uses this same function and would otherwise agree with it."""
    if value is None:
        return "\\N"
    if typ == "int" and type(value) is int:
        return str(value)
    if typ == "bool" and type(value) is int and value in (0, 1):
        return "t" if value else "f"
    if typ == "bytea" and isinstance(value, bytes):
        return "\\\\x" + value.hex()
    if typ == "text" and isinstance(value, str):
        return value.translate(_ESC_TABLE)
    if typ == "date" and isinstance(value, str):
        try:
            if datetime.date.fromisoformat(value).isoformat() == value:
                return value
        except ValueError:
            pass
    raise Unsupported(f"{where}: {type(value).__name__} {value!r} is not a lossless {typ}")


def ro_connect(path):
    """Read-only handle; the path is URI-quoted (`?`, `#`, `%` are safe)."""
    return sqlite3.connect(f"file:{urllib.parse.quote(os.path.abspath(path))}?mode=ro", uri=True)


def raw_io_day(path):
    day = os.path.basename(path)[:10]
    datetime.date.fromisoformat(day)  # raises on a non-day file name
    return day


def sqlite_rows(kind, spec, path):
    """COPY-text lines of one table's SQLite rows, in verification order."""
    con = ro_connect(path)
    try:
        cols = [(c, t) for c, t in spec["cols"] if not (kind == "raw-io" and c == "day")]
        prefix = [raw_io_day(path)] if kind == "raw-io" else []
        sql = f"SELECT {', '.join(c for c, _ in cols)} FROM {spec['table']} ORDER BY {', '.join(spec['order'])}"
        for i, row in enumerate(con.execute(sql)):
            fields = [enc(v, t, f"{spec['table']}.{c} (row {i + 1} in order)") for v, (c, t) in zip(row, cols)]
            yield "\t".join(prefix + fields) + "\n"
    finally:
        con.close()


def cmd_load(args):
    sys.stdout.reconfigure(encoding="utf-8")
    out = sys.stdout
    out.write("\\set ON_ERROR_STOP on\nBEGIN;\n")
    if args.kind == "raw-io":
        day = raw_io_day(args.file)
        nxt = (datetime.date.fromisoformat(day) + datetime.timedelta(days=1)).isoformat()
        part = "raw_io_" + day.replace("-", "_")
        out.write(
            f"CREATE TABLE {part} PARTITION OF raw_io FOR VALUES FROM ('{day}') TO ('{nxt}');\n"
        )
    for spec in KINDS[args.kind]:
        out.write(f"COPY {spec['table']} ({', '.join(c for c, _ in spec['cols'])}) FROM STDIN;\n")
        n = 0
        try:
            for line in sqlite_rows(args.kind, spec, args.file):
                out.write(line)
                n += 1
        except Unsupported:
            # End the COPY and roll the whole load back explicitly, so psql
            # never commits the rows already streamed.
            out.write("\\.\nROLLBACK;\n")
            out.flush()
            raise
        out.write("\\.\n")
        print(f"{spec['table']}: {n} rows emitted", file=sys.stderr)
    out.write("COMMIT;\n")


def pg_digest(kind, spec, path, psql):
    cols = ", ".join(c for c, _ in spec["cols"])
    order = ", ".join(c + (' COLLATE "C"' if c in TEXT_ORDER else "") for c in spec["order"])
    where = f" WHERE day = '{raw_io_day(path)}'" if kind == "raw-io" else ""
    query = f"COPY (SELECT {cols} FROM {spec['table']}{where} ORDER BY {order}) TO STDOUT"
    # Pin the output settings the encoding above mirrors.
    pin = "SET bytea_output = 'hex'; SET DateStyle = 'ISO, MDY'; SET client_encoding = 'UTF8';"
    proc = subprocess.Popen(shlex.split(psql) + ["-X", "-q", "-v", "ON_ERROR_STOP=1", "-c", pin, "-c", query],
                            stdout=subprocess.PIPE)
    h, n = hashlib.sha256(), 0
    for line in proc.stdout:
        h.update(line)
        n += 1
    if proc.wait() != 0:
        raise SystemExit(f"psql failed with status {proc.returncode}")
    return n, h.hexdigest()


def cmd_verify(args):
    tables, equal = [], True
    for spec in KINDS[args.kind]:
        h, n = hashlib.sha256(), 0
        for line in sqlite_rows(args.kind, spec, args.file):
            h.update(line.encode("utf-8"))
            n += 1
        pg_n, pg_sha = pg_digest(args.kind, spec, args.file, args.psql)
        same = (n, h.hexdigest()) == (pg_n, pg_sha)
        equal = equal and same
        tables.append({"table": spec["table"], "sqlite_rows": n, "pg_rows": pg_n,
                       "rows_sha256_sqlite": h.hexdigest(), "rows_sha256_pg": pg_sha, "equal": same})
    result = {"kind": args.kind, "file": args.file, "tables": tables}
    if args.kind == "raw-io" and args.manifest:
        m = json.load(open(args.manifest))
        con = ro_connect(args.file)
        lh, ln = hashlib.sha256(), 0
        for (line,) in con.execute("SELECT line FROM raw_io ORDER BY seq"):
            lh.update(bytes(line))
            lh.update(b"\n")
            ln += 1
        con.close()
        same = ln == m["rows"] and lh.hexdigest() == m["plain_sha256"]
        result["manifest"] = {"rows": m["rows"], "plain_sha256": m["plain_sha256"],
                              "lines": ln, "lines_sha256": lh.hexdigest(), "equal": same}
        equal = equal and same
    result["equal"] = equal
    print(json.dumps(result, indent=1))
    return 0 if equal else 1


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    for name in ("load", "verify"):
        s = sub.add_parser(name)
        s.add_argument("kind", choices=sorted(KINDS))
        s.add_argument("file")
        if name == "verify":
            s.add_argument("--psql", required=True, help='e.g. "docker exec -i llmux-pg psql -U llmux -d llmux"')
            s.add_argument("--manifest", help="raw-io: archive manifest of a single-generation day")
    args = p.parse_args()
    try:
        if args.cmd == "load":
            cmd_load(args)
            return 0
        return cmd_verify(args)
    except Unsupported as err:
        print(f"refusing a lossy copy: {err}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
