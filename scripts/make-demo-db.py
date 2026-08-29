#!/usr/bin/env python3
"""Builds the playground's demo database.

Shaped like Firefox's places.sqlite on purpose: the pitch is "open a .db file
you already have and cannot look inside", and the file the visitor meets should
look like one of those rather than like a tutorial table.

Written by Python's own sqlite3 for the same reason the test fixtures are —
a file zql wrote and then read would only prove it agrees with itself.

Usage:  python scripts/make-demo-db.py web/fixtures
"""
import os
import random
import sqlite3
import sys

out = sys.argv[1] if len(sys.argv) > 1 else "web/fixtures"
os.makedirs(out, exist_ok=True)

db = os.path.join(out, "places.sqlite")
if os.path.exists(db):
    os.remove(db)

con = sqlite3.connect(db)
con.executescript(
    """
    CREATE TABLE moz_places (
        id              INTEGER PRIMARY KEY,
        url             TEXT,
        title           TEXT,
        visit_count     INTEGER DEFAULT 0,
        last_visit_date INTEGER,
        frecency        REAL
    );
    CREATE TABLE moz_bookmarks (
        id     INTEGER PRIMARY KEY,
        fk     INTEGER,
        title  TEXT,
        folder TEXT
    );
    -- An index alongside the tables, so the b-tree walk has something it must
    -- skip rather than read as rows.
    CREATE INDEX moz_places_url ON moz_places(url);
    """
)

sites = [
    ("github.com", "GitHub"),
    ("news.ycombinator.com", "Hacker News"),
    ("doc.rust-lang.org", "Rust documentation"),
    ("stackoverflow.com", "Stack Overflow"),
    ("en.wikipedia.org", "Wikipedia"),
    ("sqlite.org", "SQLite"),
    ("postgresql.org", "PostgreSQL"),
]

rng = random.Random(20260828)
rows = []
for i in range(1, 421):
    host, name = rng.choice(sites)
    rows.append(
        (
            i,
            f"https://{host}/page/{i}",
            f"{name} — article {i}",
            rng.randint(1, 140),
            1_724_000_000 + i * 3607,
            round(rng.uniform(0, 2500), 4),
        )
    )
# One row with a NULL title and one with an empty one, because the difference
# between them is a thing zql is careful about and a thing the page can show.
rows.append((421, "https://example.invalid/untitled", None, 3, 1_724_500_000, 12.5))
rows.append((422, "https://example.invalid/blank", "", 1, 1_724_500_100, 0.0))

con.executemany("INSERT INTO moz_places VALUES (?,?,?,?,?,?)", rows)
con.executemany(
    "INSERT INTO moz_bookmarks VALUES (?,?,?,?)",
    [(i, i * 5, f"bookmark {i}", "toolbar" if i % 3 else "menu") for i in range(1, 61)],
)
con.commit()
con.close()

# The reading list, which is the demo's entire point in one small file.
#
# "Which of the links people sent me have I actually read?" needs a CSV and a
# SQLite database in the same query, and there is no ordinary tool that answers
# it. Two of these were never visited, so the LEFT JOIN has something real to
# leave unmatched — and COUNT skipping NULLs is what makes those come back 0
# rather than 1.
READING_LIST = [
    ("The Rust Book",          "priya", "%doc.rust-lang.org%"),
    ("SQLite file format",     "sam",   "%sqlite.org%"),
    ("Postgres wire protocol", "priya", "%postgresql.org%"),
    ("That HN thread",         "alex",  "%ycombinator%"),
    ("Wikipedia rabbit hole",  "sam",   "%wikipedia.org%"),
    ("Paper I promised to read", "alex", "%arxiv.org%"),
    ("Blog post about DNS",    "priya", "%blog.example.net%"),
]
with open(os.path.join(out, "reading-list.csv"), "w", encoding="utf-8", newline="") as f:
    f.write("title,sent_by,url_pattern\n")
    for title, sender, pattern in READING_LIST:
        f.write(f"{title},{sender},{pattern}\n")

# An ordinary file so `files('/demo')` has something recognisable to list.
with open(os.path.join(out, "README.txt"), "w", encoding="utf-8") as f:
    f.write(
        "These files are mounted into an in-memory filesystem in your browser.\n"
        "zql reads them with the same code it uses to read a real disk.\n"
    )

size = os.path.getsize(db)
print(f"  wrote {db} ({size:,} bytes, {len(rows)} history rows)")
