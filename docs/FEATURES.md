# zql — the complete feature list

> Everything planned, in one place, with each item marked **Core** (ships or the project
> has failed), **Planned** (ships unless the clock says otherwise), or **Stretch** (only if
> a gate closes early). Cut order is at the bottom and is binding.

---

## 1. The product, in one line

> **Open any `.db` file and query it with SQL. Your browser history, your app data, your
> phone backup. Nothing to install.**

Start `zql`, point `psql` at it, and query SQLite files, CSVs, JSON and your filesystem
with one language — **and join across them**.

Two audiences, and the demo serves them in this order:

1. **The legible one** — a `.db` file the viewer also has and cannot open. This is the
   cold open because it needs no explanation.
2. **The real one** — a developer joining a SQLite table against a CSV against the
   filesystem. This is the feature that would make this judge panel install the tool, and
   it is what the demo lands hardest on.

---

## 2. Server & protocol

| # | Feature | Status | Notes |
|---|---|---|---|
| 2.1 | Postgres v3 wire protocol, simple `Query` | **Core** | Verified live against `psql` 16.2 — see `SPIKE-WIRE.md` |
| 2.2 | SSLRequest / GSSENCRequest → bare `N` | **Core** | Miss this and the connection hangs with **no error**. Built first, at gate G0 |
| 2.3 | Trust authentication (`'R'` + Int32 0) | **Core** | No crypto involved |
| 2.4 | 11 `ParameterStatus` pairs | **Core** | `server_version`, `client_encoding`, `standard_conforming_strings`, … Clients misbehave without them |
| 2.5 | `BackendKeyData` + **CancelRequest** | **Core** | Ctrl-C in `psql`. **Without it `tail()` can never be stopped** — see §7. Spiked ✅ against real psql |
| 2.6 | `ErrorResponse` with SQLSTATE, position, hint | **Core** | `psql` renders the caret line natively — big polish for one field |
| 2.7 | Thread per connection | **Core** | `std::thread`, blocking IO, no async runtime |
| 2.8 | Text result format | **Core** | Binary format is optional in the protocol and confirmed unnecessary |
| 2.9 | Terminate / client-disconnect handling | **Core** | Stop immediately on write error rather than spinning at a dead socket |
| 2.10 | Extended protocol (Parse/Bind/Execute) | ❌ **Out of scope** | This is why **`psql` and node-postgres are promised; GUI clients are not** |

**Client compatibility promise:** `psql`, and `node-postgres`. DBeaver/TablePlus generally
need the extended protocol — if one works on the day it is a bonus discovered on camera,
never a rehearsed claim.

---

## 3. Data sources

| # | Source | Status | Signature |
|---|---|---|---|
| 3.1 | **`sqlite()`** | **Core — the headline** | `sqlite('file.db', 'table')` |
| 3.2 | `files` | **Core** | `files` or `files('path')` → `path, name, ext, size, modified, is_dir, depth` |
| 3.3 | `env` | **Core** | `env` → `name, value`. Nearly free, good for `SHOW SOURCES` |
| 3.4 | `csv()` | **Planned** | `csv('file.csv')`; header required; types sniffed from first 100 rows |
| 3.5 | `tail()` | **Planned** | `tail('file.log')`; streaming, never ends |
| 3.6 | `json()` | **Stretch — first to cut** | array of flat objects only |
| 3.7 | `git()` | ❌ **Cut** | Replaced by `sqlite()`, which is more useful and more impressive for +100 lines |

### 3.1 detail — what `sqlite()` actually does

Spiked and **passed first try** — see `SPIKE-SQLITE.md`.

- File header parse, page size, reserved-byte handling
- B-tree walk: leaf table pages (`0x0d`) and interior table pages (`0x05`); index pages
  identified and skipped
- Varint decoding (big-endian, 1–9 bytes)
- Record format: all serial types — NULL, 1/2/3/4/6/8-byte ints, f64, the 0 and 1
  constants, blobs and text
- **Overflow-page chain reassembly** — the trap in the format, verified byte-exact on a
  30,000-character value
- Schema from `sqlite_master`, with column names parsed out of the raw `CREATE TABLE` text
  **reusing our own SQL lexer**
- SQLite type-affinity resolution → Postgres OIDs
- **`INTEGER PRIMARY KEY` → rowid substitution.** Found by oracle comparison, not by
  reading the spec. Skip it and every primary key silently reads as `NULL`
- **WAL detection.** A live WAL database has committed rows only in the `-wal` sidecar.
  Silently returning stale-but-plausible rows is the worst failure available here, so:
  refuse loudly by default *(reading the sidecar is a stretch, ~150 lines)*

---

## 4. SQL language

Full grammar and semantics are frozen in `SQL-SUBSET.md`. Summary:

| # | Feature | Status |
|---|---|---|
| 4.1 | `SELECT` / `FROM` / `WHERE` / `LIMIT` / `OFFSET` | **Core** |
| 4.2 | Expressions: arithmetic, comparison, `AND`/`OR`/`NOT`, `||` | **Core** |
| 4.3 | `LIKE`, `IN`, `BETWEEN`, `IS NULL` | **Core** |
| 4.4 | **Three-valued NULL logic** | **Core** — `WHERE` admits only exactly `TRUE` |
| 4.5 | `GROUP BY` + `HAVING` | **Core** |
| 4.6 | Aggregates: `COUNT`/`SUM`/`AVG`/`MIN`/`MAX`, `COUNT(DISTINCT)` | **Core** |
| 4.7 | `ORDER BY` with `ASC`/`DESC`, `NULLS FIRST`/`LAST` | **Core** |
| 4.8 | **`JOIN` — `INNER` and `LEFT`** | **Core** — *this is the utility feature; cut almost anything before it* |
| 4.9 | Scalar functions (16, closed list) | **Planned** |
| 4.10 | `CAST`, `CASE WHEN` | **Planned** |
| 4.11 | `DISTINCT` | **Planned** |
| 4.12 | `SHOW SOURCES` | **Planned** — the discoverability answer to "what can I even query?" |
| 4.13 | `EXPLAIN` | **Stretch** — ~40 lines, strong Code Quality signal |
| 4.14 | Subqueries, CTEs, windows, `UNION`, DML, transactions | ❌ **Out of scope** — each returns `0A000` naming the feature |

**Read-only is a product boundary, not an apology.** It removes transactions, locking,
constraint checking and write-ahead logging from scope in one stroke, and none appear in
the demo.

---

## 5. Engine

| # | Feature | Status | Why |
|---|---|---|---|
| 5.1 | Pull-based (volcano) operator tree | **Core** | `LIMIT` and streaming fall out for free; textbook-recognisable |
| 5.2 | Bind phase: names → sources, columns → **indices** | **Core** | `RowDescription` must precede `DataRow`, so the schema must be known up front |
| 5.3 | Compiled expression tree | **Core** | No per-row string lookups. An enum, not `Box<dyn Fn>`, so it stays debuggable |
| 5.4 | 7 operators: scan, filter, project, sort, aggregate, limit, join | **Core** | |
| 5.5 | Hash aggregate | **Core** | `f64` group keys hash on `to_bits`, NaN canonicalised |
| 5.6 | In-memory sort with a documented row ceiling | **Core** | External sort **cut** as over-engineering — saves ~100 lines |
| 5.7 | **Filesystem index cached per session** | **Planned** | 5 s → ~50 ms on the second query. A large demo win for ~80 lines |
| 5.8 | Cancellation flag checked between rows | **Core** | See §7 |
| 5.9 | **Never panic** | **Core** | Checked indexing and arithmetic throughout; `catch_unwind` at the connection boundary as a last resort |

---

## 6. Interface & operations

| # | Feature | Status |
|---|---|---|
| 6.1 | CLI: `--port`, `--host`, `--dir`, `--no-cache`, `--help` | **Core** |
| 6.2 | **Live dashboard** — HTTP + SSE, one self-contained page showing the query log | **Planned, protected** — spiked ✅, verified against Node and a real Chrome `EventSource` |
| 6.2a | Dashboard heartbeat (`: ping` every ~15 s) | **Planned** — required for pruning; found by spiking, see `SPIKE-DASHBOARD.md` |
| 6.3 | Structured error messages with caret and `HINT: did you mean FROM?` | **Planned** |
| 6.4 | `README.md` with an honest limitations section | **Core** |
| 6.5 | `STDLIB.md` — 13 substitutions with rationales | **Core** |
| 6.6 | Reproducible build, two hashes published | **Core** |

**On the dashboard:** it is on the cut list on paper and protected in practice. The
prior-art corpus is unambiguous that static projects lose, and it is worth roughly 0.5 on
the demo diagnostic. Cut `json()`, `tail()`, and half the CSV sniffer first.

---

## 7. The feature that is easy to miss

**CancelRequest is not optional.** `tail()` never ends. In `psql`, Ctrl-C does not kill the
connection — it opens a *second* TCP connection and sends a `CancelRequest` carrying the
PID and secret key we handed out in `BackendKeyData`. If the server ignores that, the
live-streaming query — the single best piece of motion in the whole demo — cannot be
stopped, and the video ends on a hung terminal.

~60 lines: a `HashMap<i32, Arc<AtomicBool>>` keyed by our fake PID, checked between rows,
returning SQLSTATE `57014`.

---

## 8. Budget and cut order

**5,290 lines designed. ~42 building hours.** That is above comfort, so the cut order is
decided now, in writing, while it is a calm decision rather than an hour-38 one:

| Order | Cut | Saves | Cost of cutting |
|---|---|---|---|
| 1 | `json()` | ~250 | None real — `csv()` covers the same demo beat, and JSON shows no craft `sqlite()` hasn't |
| 2 | CSV type sniffing → all-Text | ~60 | Uglier output in the join demo |
| 3 | `tail()` + cancellation | ~200 | Loses the best motion in the video. Painful |
| 4 | `EXPLAIN`, `CASE`, `BETWEEN`, `IN`, `DISTINCT`, `HAVING` | ~250 | Individually small |
| 5 | Dashboard | ~400 | **Resist.** Static projects lose |
| 6 | `JOIN` | ~300 | **Last resort.** This is the utility |

**Floor: ~4,590 lines** with join and dashboard gone. That is the version that still wins
something. The target is the full 5,290 minus cut #1.

---

## 9. Stated limitations — these go in the README verbatim

Writing these down as features of the pitch, not as embarrassments. An honest limitations
section reads as confidence; a missing one reads as a project that hasn't been tested.

- **Read-only.** No writes, no transactions, no DDL.
- **Simple query protocol only.** `psql` and node-postgres work. GUI clients that require
  Parse/Bind/Execute do not.
- **No index use.** Full table scans only. Fine at these sizes — but said out loud.
- **WAL-mode SQLite files must be checkpointed**, or zql refuses. It will not return stale
  rows quietly.
- **Encrypted or extension-modified SQLite files** (nonzero reserved bytes) are out of scope.
- **UTC only.** No time zone database in std.
- **In-memory sort and aggregation.** There is a row ceiling and it errors above it rather
  than swapping the machine to death.
- **The backend secret key is not cryptographically random.** It is `SystemTime` nanos and
  a counter, and it protects nothing that matters.
- **`LIKE` is case-sensitive** (Postgres semantics), which differs from SQLite's default.
