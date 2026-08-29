#!/usr/bin/env python3
"""Generates the SQLite fixtures, and prints the values the Rust tests assert.

Python's `sqlite3` is the oracle: it writes these files, so anything zql reads
back that differs from what is written here is a bug in zql's reader.

    python tests/fixtures/generate.py

The generated `.db` files are committed. Test *data* is not a dependency, and
committing them means `cargo test` needs nothing but Rust.
"""

import os
import sqlite3
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def fresh(name):
    path = os.path.join(HERE, name)
    for suffix in ("", "-wal", "-shm", "-journal"):
        if os.path.exists(path + suffix):
            os.remove(path + suffix)
    return path


def build_simple():
    """An ordinary file: 4096-byte pages, an INTEGER PRIMARY KEY, an overflow."""
    path = fresh("simple.db")
    db = sqlite3.connect(path)
    db.execute("PRAGMA page_size = 4096")
    db.execute("PRAGMA journal_mode = DELETE")

    db.execute(
        "CREATE TABLE users ("
        "  id INTEGER PRIMARY KEY,"
        "  name TEXT,"
        "  score REAL,"
        "  active INT"
        ")"
    )
    db.executemany(
        "INSERT INTO users (id, name, score, active) VALUES (?, ?, ?, ?)",
        [(n, "user_%d" % n, n * 1.5, n % 2) for n in range(1, 501)],
    )

    # A 9,000-byte value in 4,096-byte pages: the payload must span an
    # overflow chain, and the first and last bytes prove it was reassembled.
    db.execute("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)")
    long_text = "".join(
        "ZkyNAjjNdmvjzUkg"[(i * 7) % 16] for i in range(9000 - 32)
    )
    long_text = "ZkyNAjjNdmvjzUkg" + long_text + "ZQBmwnrMeYjldNYu"
    db.executemany(
        "INSERT INTO notes (id, body) VALUES (?, ?)",
        [(n, long_text if n == 7 else "short note %d" % n) for n in range(1, 21)],
    )

    db.commit()
    db.close()
    return path


def build_hard():
    """The awkward one: 8 KB pages, an index, Unicode, the integer extremes."""
    path = fresh("hard.db")
    db = sqlite3.connect(path)
    db.execute("PRAGMA page_size = 8192")
    db.execute("PRAGMA journal_mode = WAL")

    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT, i INT, f REAL, b BLOB)")
    db.execute("CREATE INDEX idx_s ON t(s)")
    db.executemany(
        "INSERT INTO t (id, s, i, f, b) VALUES (?, ?, ?, ?, ?)",
        [
            (1, "", -1, 0.0, None),
            (2, "héllo wörld \U0001f39e", -9223372036854775808, -1.5e300, b"x"),
            (3, "A" * 30000, 9223372036854775807, 3.141592653589793, None),
        ],
    )

    # A Unicode table name, and a table with no rows at all.
    db.execute('CREATE TABLE "写真" (id INTEGER PRIMARY KEY, label TEXT)')
    db.execute('INSERT INTO "写真" (label) VALUES (?)', ("ユーザー",))
    db.execute("CREATE TABLE empty_table (a INTEGER, b TEXT)")

    # Mixed-case column names, which must survive zql's identifier folding.
    db.execute('CREATE TABLE quirks (visitCount INTEGER, "My Column" TEXT, plain)')
    db.execute('INSERT INTO quirks VALUES (?, ?, ?)', (42, "kept", None))

    db.commit()
    # Checkpoint, or the committed rows live only in the -wal sidecar and zql
    # will (correctly) refuse to read the file.
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    db.close()
    return path


def build_wide():
    """Enough rows to force interior b-tree pages, so the walk must descend."""
    path = fresh("wide.db")
    db = sqlite3.connect(path)
    db.execute("PRAGMA page_size = 512")
    db.execute("PRAGMA journal_mode = DELETE")
    db.execute("CREATE TABLE many (id INTEGER PRIMARY KEY, filler TEXT)")
    db.executemany(
        "INSERT INTO many (id, filler) VALUES (?, ?)",
        [(n, "row %d %s" % (n, "." * 40)) for n in range(1, 4001)],
    )
    db.commit()
    db.close()
    return path


def report(path):
    """Prints what the oracle sees, which is what the Rust tests assert."""
    db = sqlite3.connect(path)
    print("\n=== %s (%d bytes) ===" % (os.path.basename(path), os.path.getsize(path)))
    tables = [
        row[0]
        for row in db.execute(
            "SELECT name FROM sqlite_master WHERE type='table' "
            "AND name NOT LIKE 'sqlite_%' ORDER BY name"
        )
    ]
    for table in tables:
        count = db.execute('SELECT count(*) FROM "%s"' % table).fetchone()[0]
        print("  %-14s %d rows" % (table, count))
    db.close()


def main():
    paths = [build_simple(), build_hard(), build_wide(), build_altered()]
    for path in paths:
        report(path)

    db = sqlite3.connect(paths[0])
    body = db.execute("SELECT body FROM notes WHERE id = 7").fetchone()[0]
    print("\n=== oracle values the Rust tests assert ===")
    print("simple.db users id=250 ->", db.execute(
        "SELECT name, score, active FROM users WHERE id = 250").fetchone())
    print("simple.db notes id=7 length ->", len(body))
    print("simple.db notes id=7 first 16 ->", body[:16])
    print("simple.db notes id=7 last 16  ->", body[-16:])
    db.close()

    db = sqlite3.connect(paths[1])
    print("hard.db t id=2 ->", db.execute(
        "SELECT s, i, f FROM t WHERE id = 2").fetchone())
    print("hard.db t id=3 len(s) ->", db.execute(
        "SELECT length(s) FROM t WHERE id = 3").fetchone()[0])
    db.close()


def build_altered():
    """A table whose rows predate one of its columns.

    `ALTER TABLE ADD COLUMN` does not rewrite existing rows, so records written
    before the column existed simply end early. A reader that assumes every
    record has every column reads past the end of one.
    """
    path = fresh("altered.db")
    db = sqlite3.connect(path)
    db.execute("PRAGMA journal_mode = DELETE")
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT)")
    db.executemany("INSERT INTO t (id, a) VALUES (?, ?)", [(1, "old"), (2, "older")])
    db.commit()
    # Rows 1 and 2 now have two columns; rows added after have three.
    db.execute("ALTER TABLE t ADD COLUMN b TEXT")
    db.execute("INSERT INTO t (id, a, b) VALUES (3, 'new', 'present')")
    db.commit()
    db.close()
    return path


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    main()
