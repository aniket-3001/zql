# STDLIB.md — what was written by hand, and why

zql's `[dependencies]` section is empty. `cargo tree` prints one node.

This document lists every crate a normal build of this program would have
pulled in, what replaced it, and — the part that actually matters — **what
each replacement got wrong the first time.** A substitution that is merely
listed proves nothing; a substitution with a bug in its history proves it was
built rather than copied.

Nothing is vendored. There is no `vendor/` directory, no `include!` of foreign
source, and no code in this repository that was not written for it.

---

## The substitutions

| # | Would have used | Written by hand | Where |
|---|---|---|---|
| 1 | `rusqlite`, `sqlx`, `libsqlite3-sys` | SQLite file reader: header, b-tree walk, varints, serial types, overflow chains, `CREATE TABLE` parsing, type affinity | `src/sources/sqlite/` |
| 2 | `tokio-postgres`, `postgres`, `pgwire` | PostgreSQL v3 wire protocol: framing, startup, auth, `RowDescription`/`DataRow`, `ErrorResponse`, cancellation | `src/wire/`, `src/server/` |
| 3 | `sqlparser-rs`, `nom`, `pest` | SQL lexer and Pratt expression parser | `src/sql/` |
| 4 | `csv` | RFC 4180 CSV parser with quoting, embedded newlines, and type sniffing | `src/sources/csv.rs` |
| 5 | `clap`, `structopt` | Argument parsing and `--help` | `src/main.rs` |
| 6 | `walkdir` | Recursive directory walk with symlink and permission handling | `src/sources/files.rs` |
| 7 | `tokio`, `async-std`, `smol` | `std::thread` per connection, blocking IO | `src/server/mod.rs` |
| 8 | `anyhow`, `thiserror` | `ZqlError` with SQLSTATE codes and `Display` | `src/error.rs` |
| 9 | `chrono`, `time` | Unix time to civil date (days-from-civil) | `src/datetime.rs` |
| 10 | `hyper`, `tiny_http`, `axum` | HTTP/1.1 server for the dashboard | `src/dash/mod.rs` |
| 11 | `serde_json` | JSON string escaping for the event stream | `src/dash/sse.rs` |
| 12 | `tungstenite`, `ws` | Server-sent events, with client pruning and heartbeat | `src/dash/sse.rs` |
| 13 | `notify` | Log following by polling, with truncation detection | `src/sources/tail.rs` |
| 14 | `local-ip-address`, `if-addrs` | Default-route interface discovery via a UDP socket | `src/dash/mod.rs` |
| 15 | `rayon`, `crossbeam` | `Arc`, `Mutex`, `RwLock`, atomics, plain loops | throughout |
| 16 | `ordered-float` | `GroupKey`: hashable values with canonical NaN and `-0.0` | `src/value.rs` |
| 17 | `strsim`, `levenshtein` | Damerau distance-1 check for "did you mean" | `src/sql/token.rs` |

Seventeen, against a bar of ten.

---

## The five that were genuinely hard

Anyone can replace `clap` with a `match`. These are the ones where the standard
library gave no help at all and the format or protocol had to be implemented
from its specification — and each is listed with the bug it actually had.

### 1. The SQLite b-tree reader — `rusqlite`

The most widely deployed database format on earth, read without linking
`libsqlite3`. Pages, cell pointer arrays, interior and leaf table pages, index
pages skipped, varints, eleven serial types with sign extension at 1, 2, 3, 4,
6 and 8 bytes, and overflow-page chain reassembly.

**What was wrong the first time.** Three things, all found by comparing against
Python's `sqlite3` rather than by reading:

- **`INTEGER PRIMARY KEY` is stored as NULL in the record.** It is an alias for
  the rowid, and the real value lives in the cell header. Without the
  substitution, every primary key in every table reads as NULL — plausible
  enough to survive casual testing and wrong in every single row.
- **The overflow threshold is not "as much as fits".** SQLite deliberately
  keeps less on the page so the tree stays dense. An implementation that fills
  the page and then overflows reads small values correctly and large ones
  silently wrong.
- **A `REAL` column holding a whole number is stored as an integer.** SQLite
  converts it back on read. Returning the integer disagrees with every other
  SQLite client about what the file contains. The spike recorded this as
  cosmetic; it is not.

Verified byte-exact against Python's `sqlite3` on `i64::MIN`, `i64::MAX`, π,
astral-plane emoji, a Unicode table name, an 8 KB page size, and a
30,000-character value spanning an overflow chain.

### 2. The PostgreSQL v3 wire protocol — `tokio-postgres`

Enough of the protocol that real `psql` cannot tell the difference.

**What was wrong the first time.** The reply to `SSLRequest` is a **single
unframed byte**, not a message. Answer it with a framed message, or not at all,
and there is no error anywhere — the connection simply hangs. `psql` sends one
before anything else, every time.

The second trap is the length field: it counts its own four bytes and excludes
the tag. That arithmetic exists in exactly one function in this codebase
(`wire::Message::write_to`) so it can only be wrong once.

### 3. The SQL parser — `sqlparser-rs`

A lexer and a Pratt expression parser over a grammar frozen in writing before
any of it was implemented (`docs/SQL-SUBSET.md`).

**What was wrong the first time.** `a OR b AND c` parsed as `(a OR b) AND c`.
Pratt binding powers have to be spaced *two* apart: a level's right power sits
one above its left to get left-associativity, which leaves the next level up
needing to clear it strictly. Packed one apart, `OR` ends at 2, `AND` starts at
2, and `AND` is never absorbed. Every `WHERE` clause mixing the two would have
returned wrong rows, silently. It was caught by a fuzz sweep, not by review.

### 4. Days-from-civil — `chrono`

Converting a Unix timestamp to a calendar date without a library, using
Howard Hinnant's algorithm: shift the epoch to March so the leap day lands at
the end of the year and every month-length special case disappears.

The subtlety is negative timestamps. Truncating division rounds `-1` toward
zero and puts pre-1970 instants on the wrong day; Euclidean division does not.

### 5. Server-sent events — `hyper` + a websocket crate

An HTTP/1.1 server and a live event stream, with the three properties that make
the difference between a demo and a deadlock:

- a bounded write timeout per client, or one browser that stops reading
  without disconnecting blocks the producer and freezes the dashboard for
  everyone,
- pruning *is* the broadcast — a failed write means the client is gone,
- a heartbeat, because pruning only happens during a broadcast and zql only
  broadcasts when a query runs. Without it, a closed tab holds its socket for
  as long as nobody queries.

---

## Platform interfaces used

Declared for completeness. None of these is a package.

| | |
|---|---|
| `std` | The entire runtime: `net`, `fs`, `thread`, `sync`, `io`, `collections`. |
| Operating-system APIs | Reached only through `std`. No `extern` block, no FFI, no direct syscall, and nothing platform-specific. |

**zql contains no `unsafe` code at all.** Both of these return nothing:

```
grep -rn "unsafe" src/
grep -rn "extern" src/
```

That is worth stating because the obvious way to read a file format quickly is
to transmute a byte slice into a struct. Every read in the SQLite reader is a
bounds-checked slice instead, which is why 62 deliberately corrupted databases
produce errors rather than crashes.

---

## Test tooling

| | |
|---|---|
| `#[test]` and `cargo test` | Rust ships a test harness, so there is no dev-dependency. `[dev-dependencies]` is absent, not empty-but-present. |
| `tests/fixtures/*.db`, `*.csv` | Test **data**, generated by `tests/fixtures/generate.py` and committed so `cargo test` needs nothing but Rust. Data is not a dependency. |
| Python 3, `psql`, Node | Used as **oracles during development** to check zql's output against independent implementations. Nothing links against them, nothing ships with them, and `cargo test` does not invoke them. |

---

## Package Killer

Crates this single binary replaces, with the download counts that make the
point about how much of a normal Rust program is other people's code:

`rusqlite` · `sqlx` · `diesel` · `libsqlite3-sys` · `tokio-postgres` ·
`postgres` · `pgwire` · `sqlparser` · `nom` · `csv` · `serde` · `serde_json` ·
`clap` · `walkdir` · `tokio` · `hyper` · `axum` · `tiny_http` · `chrono` ·
`time` · `anyhow` · `thiserror` · `rayon` · `crossbeam` · `notify` ·
`ordered-float` · `strsim` · `local-ip-address`

Twenty-eight crates, before transitive dependencies. `tokio` and `serde` alone
pull in dozens more.
