# zql — architecture

---

## 1. System shape

```
                 TCP :5432
                     │
              ┌──────▼───────┐
              │   listener   │  thread per connection
              └──────┬───────┘
                     │
              ┌──────▼───────┐
              │   startup    │  SSLRequest → 'N' · StartupMessage → params
              └──────┬───────┘
                     │
        ┌────────────▼────────────┐
        │        session          │  loop: read 'Q', run, write 'Z'
        └────────────┬────────────┘
                     │  SQL text
        ┌────────────▼────────────┐
        │   lexer → parser → AST  │        sql/
        └────────────┬────────────┘
                     │  Statement
        ┌────────────▼────────────┐
        │         binder          │        plan/
        │  names → sources        │  ← the only place that knows both
        │  exprs → CompiledExpr   │     the AST and the catalogue
        │  produces Schema        │
        └────────────┬────────────┘
                     │  Plan + Schema
        ┌────────────▼────────────┐
        │   operator tree (pull)  │        exec/
        └────────────┬────────────┘
                     │  Row stream
        ┌────────────▼────────────┐
        │  RowDescription 'T'     │        wire/
        │  DataRow 'D' × n        │
        │  CommandComplete 'C'    │
        └─────────────────────────┘
```

**The one invariant that shapes everything:** `RowDescription` must be written **before**
the first `DataRow`. So the full output schema has to be known before execution begins.
That is why binding is a distinct phase and not something operators discover as they run.

---

## 2. Module tree

Single-file was declined — 4,600 lines in one file costs more on Code Quality (25%) than
the +5 bonus is worth. The tree maps 1:1 onto the costing table so progress is legible.

```
src/
  main.rs              CLI parsing, config, listener bootstrap
  error.rs             ZqlError, SQLSTATE mapping, Display
  value.rs             Value, coercion, three-valued comparison, text encoding

  wire/
    mod.rs             Message framing: tag + Int32 len + body
    backend.rs         Typed constructors for messages we send
    frontend.rs        Parsing of messages we receive
    oid.rs             Type OIDs and Value → wire-text rendering

  server/
    mod.rs             Connection lifecycle, session state
    startup.rs         SSLRequest / GSSENCRequest / CancelRequest / StartupMessage
    cancel.rs          PID+secret registry, cancellation flags

  sql/
    mod.rs
    token.rs           Token, keyword table
    lexer.rs           Text → tokens; string/identifier/number literals
    parser.rs          Statement parser + Pratt expression parser
    ast.rs             Statement, Select, Expr, OrderKey, ...

  plan/
    mod.rs             Binder: AST + catalogue → Plan + Schema
    plan.rs            Plan enum
    expr.rs            CompiledExpr, column indices resolved at bind time
    schema.rs          Schema, Column, Type

  exec/
    mod.rs             RowIter trait, operator dispatch
    scan.rs  filter.rs  project.rs
    sort.rs  aggregate.rs  limit.rs  join.rs

  sources/
    mod.rs             TableSource trait, function registry
    files.rs           recursive walk + session cache
    env.rs
    csv.rs
    json.rs
    tail.rs            streaming, never returns None
    sqlite/
      mod.rs           Db open, sqlite_master, schema extraction
      btree.rs         page walk, leaf/interior table pages
      record.rs        varints, serial types, overflow chains
      ddl.rs           CREATE TABLE mini-parser + type affinity

  dash/                (cuttable, but protected — see §9)
    mod.rs             HTTP/1.1 server
    sse.rs             event stream
    page.rs            the single self-contained HTML page
```

---

## 3. Core contracts

### 3.1 `Value`

```
enum Value { Null, Bool(bool), Int(i64), Real(f64), Text(String), Blob(Vec<u8>), Timestamp(i64) }
```

`Timestamp` is a distinct variant rather than an `Int` so that OID 1114 and date
formatting live in one place. `Bool` is distinct because `WHERE` results are boolean and
Postgres OID 16 expects `t`/`f` on the wire, not `1`/`0`.

**Three-valued logic is the trap.** `NULL = NULL` is `NULL`, not `true`. `WHERE` admits a
row only when the predicate is **exactly `Bool(true)`** — `Null` filters out. Comparison
returns `Option<Ordering>`, and every operator that consumes it must handle `None`
deliberately. This is where a hand-written engine is most likely to be quietly wrong, so
it gets its own test table.

### 3.2 `TableSource` and `RowIter`

```
trait TableSource {
    fn schema(&self) -> &Schema;
    fn scan(&self) -> Result<Box<dyn RowIter>>;
}

trait RowIter {
    fn next(&mut self) -> Result<Option<Row>>;
}
```

**Why a custom trait instead of `Iterator<Item = Result<Row>>`:** the standard iterator
forces `Option<Result<Row>>`, which inverts the natural reading — every operator would
match on "some, and it's an error" before "none". With `Result<Option<Row>>` the whole
volcano loop propagates errors with `?` and terminates on `Ok(None)`. A senior reviewer
will ask about this, so the rationale goes in a doc comment on the trait, not just here.

The cost is losing iterator adapters, which we do not use anyway — every operator is
hand-written.

### 3.3 `Row`

`Row(Vec<Value>)` — a newtype, so the representation can change without touching
operators. Columnar batching was considered and rejected: at ~10⁵ rows it buys nothing
measurable and costs a great deal of readability, and Code Quality is 25% while
performance is not a criterion at all.

---

## 4. Design decisions, with reasons

### 4.1 Pull-based (volcano) execution

Each operator exposes `next()` and pulls from its child.

**Why:** `LIMIT` falls out for free (stop pulling), `tail()` — an infinite source — works
naturally, and it is the textbook design a senior reviewer recognises on sight. Push-based
execution vectorises better and is materially harder to read. We are not optimising
throughput.

### 4.2 One `Plan` enum — no logical/physical split

```
enum Plan {
  Scan   { source: Box<dyn TableSource>, schema: Schema },
  Filter { input: Box<Plan>, pred: CompiledExpr },
  Project{ input: Box<Plan>, exprs: Vec<CompiledExpr>, schema: Schema },
  Aggregate { input: Box<Plan>, keys: Vec<CompiledExpr>, aggs: Vec<AggSpec> },
  Sort   { input: Box<Plan>, keys: Vec<SortKey> },
  Limit  { input: Box<Plan>, limit: Option<u64>, offset: u64 },
  Join   { left: Box<Plan>, right: Box<Plan>, on: CompiledExpr, kind: JoinKind },
}
```

**Why one:** a physical plan exists to choose between access paths. We have no indexes and
no join reordering, so there is exactly one physical plan per logical plan. A second tree
would be ceremony. **Document the omission** — deliberately-absent structure with a stated
reason reads better than absent structure.

### 4.3 Expressions compiled at bind time

Column references are resolved to **integer indices** during binding, producing a
`CompiledExpr` tree. The executor never looks up a column by name.

**Why:** the classic naive-interpreter mistake is a string hash per column per row. At
127k rows × 6 columns that is 750k lookups per query for no reason. Resolving once at bind
time is a few lines and visibly better engineering.

`CompiledExpr` is an enum, not a `Box<dyn Fn>` — enums are debuggable, printable in a
`--explain` mode, and avoid lifetime entanglement.

### 4.4 In-memory sort — external sort **cut**

Collect into `Vec<Row>`, sort with a comparator built from the ORDER BY keys, with a
configurable row ceiling and a clear error above it.

**This reverses an earlier decision.** The costing carried "external sort + hash aggregate,
250 lines". External sort is over-engineering for an interactive inspection tool that
returns screenfuls; nobody `ORDER BY`s ten million rows through `psql`. **Saves ~100 lines
and removes a whole class of bug.** Hash aggregate stays (~150 lines).

Note: `HashMap<Vec<Value>, ...>` needs `Value: Hash + Eq`, and `f64` is neither. Group keys
hash on `f64::to_bits`, with NaN normalised to a single canonical bit pattern. Documented,
because it is exactly the kind of silent wrongness this project must not have.

### 4.5 Cancellation must be implemented — **this reverses an earlier decision**

`OVERVIEW.md` says `CancelRequest → close the connection`. **That breaks the demo.**

`tail()` never returns `Ok(None)`. In `psql`, Ctrl-C sends a `CancelRequest` on a *separate*
TCP connection carrying the PID and secret key we handed out in `BackendKeyData`. If we
ignore it, the streaming query — the single best piece of motion in the whole demo — cannot
be stopped, and the video ends with a hung terminal.

Design: a process-wide `HashMap<i32, Arc<AtomicBool>>` behind a `Mutex`, keyed by the fake
PID. The cancel connection sets the flag; every operator checks it between rows and returns
a `57014 query_canceled` error. **~60 lines.** Not optional.

**Verified 2026-08-17 against real `psql` 16.2 with a real Ctrl-C — see
`SPIKE-CANCEL.md`.** Four details that only surfaced by building it: check between rows and
never mid-row (a half-written `DataRow` desynchronises the stream permanently); reset the
flag when a new query *starts*, not when a cancel is consumed; send `ReadyForQuery` after
the error so the session stays usable; and remove the PID from the registry on disconnect
or the map grows for the process lifetime.

Also handle the client vanishing mid-stream: a write error means stop immediately rather
than spinning against a dead socket.

### 4.8 The dashboard needs a heartbeat

**Found by spiking, not by designing — see `SPIKE-DASHBOARD.md`.**

Live clients are pruned by the broadcast itself: `retain_mut` over a `Vec<TcpStream>`, and a
failed write means gone. But zql only broadcasts **when a query runs**, so with no query
activity a closed browser tab holds its socket indefinitely.

A `: ping\n\n` comment every ~15 s from a timer thread fixes it, and does three jobs at
once: prunes dead clients regardless of query activity, stops intermediaries closing an idle
connection, and shows liveness on the demo video when nothing is happening. **~5 lines.**

Two more confirmed constraints: `set_write_timeout` on each client socket, or one client
that stops reading without disconnecting blocks the producer and freezes the dashboard for
everyone; and the `/events` handler must **end** after handing its socket to the producer,
so N dashboards do not mean N parked threads.

### 4.6 Filesystem source caches per session

The wire spike walked `D:\Aniket` fresh on every query — 127,490 rows in ~5 s. Fine as
proof, poor as a demo.

Design: build the file list **once per session**, guarded by an `Arc<RwLock<FileIndex>>`,
with `--no-cache` to force a rebuild and a visible `(cached)` note in the dashboard.
**~80 lines**, turns 5 s into tens of milliseconds for the second query onward.

This matters for the video: the first query establishes that it is doing real work, and
every query after it feels instant.

### 4.7 Threading

One `std::thread` per connection. No thread pool for connections — a hackathon demo has
single-digit clients, and a pool would be complexity for nothing.

Execution is single-threaded per query. The *only* candidate for parallelism is the
initial filesystem walk; it is on the optional list, below the dashboard.

Shared state is deliberately tiny: the cancel registry, the file index, and dashboard
stats. Each is its own lock; there is no global mutable state.

---

## 5. Error model

```
struct ZqlError { code: SqlState, message: String, detail: Option<String> }
```

Every fallible path returns `Result<T, ZqlError>`, and the type maps **directly** onto
`ErrorResponse` — no translation layer.

| Situation | SQLSTATE |
| --- | --- |
| syntax error | `42601` |
| unknown table / function | `42P01` |
| unknown column | `42703` |
| type mismatch | `42804` |
| unsupported feature (subquery, CTE, window, DML) | `0A000` |
| corrupt or unreadable source file | `58030` |
| query cancelled | `57014` |

**Never panic.** Every parse, read and index is fallible-by-construction: `.get()` over
`[]`, checked arithmetic on anything derived from file contents, no `unwrap()` outside
tests. The JPEG spike proved this discipline holds — 23 adversarial files, zero panics.
A `catch_unwind` at the connection boundary is the last-resort net so one malformed
database cannot take the server down; it converts to `XX000` and logs.

**The specific failure this project must not have:** silently returning wrong rows. A
WAL-mode SQLite file whose `-wal` sidecar we ignore returns *stale but plausible* data.
That is worse than crashing. Detect WAL mode from the header and either read the sidecar
or refuse loudly — never quietly.

---

## 6. The `sqlite()` source

Spiked and passed (see `SPIKE-SQLITE.md`). Three things the spike did **not** cover
and that the costing must absorb:

### 6.1 Column names need a `CREATE TABLE` parser

`RowDescription` needs real column names. They exist only as raw DDL text in
`sqlite_master.sql`. **We already have a SQL lexer** — reuse it. Must handle quoted
identifiers, parameterised types (`VARCHAR(255)`), inline constraints, and table-level
constraints that are not columns. **~150 lines** (`sqlite/ddl.rs`).

### 6.2 Type affinity

SQLite's declared types are advisory and resolved by substring rule, in order: contains
`INT` → INTEGER; contains `CHAR`/`CLOB`/`TEXT` → TEXT; contains `BLOB` or empty → BLOB;
contains `REAL`/`FLOA`/`DOUB` → REAL; otherwise NUMERIC. This determines the OID we
advertise.

### 6.3 `INTEGER PRIMARY KEY` substitution

Found by oracle comparison, not by reading the spec: such a column is an alias for the
rowid and is stored as `NULL` in the record. The binder must detect it from the DDL and
substitute the cell's rowid. **Skip this and every primary key reads as NULL** — plausible
enough to survive casual testing and wrong in every row.

---

## 7. Wire layer

Framing, the startup dance, message tags and OIDs are all in `OVERVIEW.md` §Protocol
reference, verified against `psql` 16.2 and node-postgres. Not repeated here.

Architectural points only:

- `wire/` knows **nothing** about SQL. It moves bytes and typed messages. This keeps the
  protocol independently testable against a recorded byte trace.
- Backend messages are constructed through typed helpers, never assembled ad hoc — the
  length-includes-itself-but-not-the-tag rule is encoded in exactly one place.
- Value → wire text lives in `wire/oid.rs`, beside the OID table, so a type and its
  rendering cannot drift apart.
- **Text format only.** Binary is optional in the protocol and confirmed unnecessary.

---

## 8. Testing strategy

Three independent oracles, all already installed:

| Layer | Oracle | Check |
| --- | --- | --- |
| Wire protocol | `psql` 16.2 **and** node-postgres | two independent implementations, no shared code |
| `sqlite()` | Python 3.11 `sqlite3` | byte-exact value comparison, including overflow chains |
| SQL semantics | golden query file | `query → expected output`, diffed |

Plus a **panic sweep**: a corpus of malformed `.db`, `.csv` and `.json` files asserting
zero panics and a specific error for each — the same shape as the JPEG spike's 23-file
adversarial run, which is the single highest-value test pattern found in this whole
process.

All of it uses `#[test]` and `tests/`, which are std. Fixtures are generated by a script
written **inside** the window.

---

## 9. Revised costing

| Module | Lines | Δ vs `OVERVIEW.md` |
| --- | --- | --- |
| wire (framing, messages, OIDs) | 400 | — |
| server (session, startup) | 250 | — |
| **cancel registry** | **60** | **+60 — newly required (§4.5), spiked ✅** |
| sql (lexer, parser, ast) | 800 | — |
| plan (binder, CompiledExpr, schema) | 600 | — |
| exec (7 operators) | 500 | — |
| hash aggregate | 150 | **−100 — external sort cut (§4.4)** |
| value + three-valued logic | 300 | — |
| sources: files + **cache** | 430 | **+80 — session cache (§4.6)** |
| sources: env, csv, json, tail | 450 | — |
| **sqlite: btree + record** | 350 | — |
| **sqlite: ddl + affinity** | **150** | **+150 — newly costed (§6.1)** |
| join | 300 | cuttable |
| dashboard (http + sse + page + heartbeat) | 405 | cuttable, **protected**, spiked ✅ |
| cli + help | 150 | — |
| **Total** | **5,290** | **+690 over the 4,600 estimate** |
| **Floor** (join + dashboard cut) | **4,590** | |

**Honest read:** designing it properly added ~690 lines, which is what designing properly
usually does. 5,290 is above comfort for 42 hours. Two responses, in order:

1. **`json()` is the first thing to cut** (~250 lines). `csv()` covers the same demo beat
   and JSON adds no craft the SQLite reader has not already shown.
2. If more is needed, cut `join` — but note §"the join is the utility" in the demo
   analysis: it is the query this judge panel would actually install the tool for. **Cut
   `json()`, `tail()` and the second half of the CSV type sniffer before touching `join`.**

The dashboard stays. It is worth ~0.5 on the demo diagnostic and the corpus is unambiguous
that static projects lose.

---

## 10. Build order — riskiest first, always demoable

Each gate ends in something that runs. If the clock stops at any gate, there is a submission.

| Gate | Hours | Deliverable | Proof |
| --- | --- | --- | --- |
| **G0** | H+0–2 | handshake only | **`psql` shows a prompt.** SSLRequest is the trap that hangs silently — it is built and tested *first*, before anything else exists |
| **G1** | H+2–4 | `SELECT 1` | full round trip: RowDescription → DataRow → CommandComplete → ReadyForQuery |
| **G2** | H+4–10 | lexer + parser | frozen subset parses; unsupported syntax gives `0A000`, not a panic |
| **G3** | H+10–14 | binder + scan/filter/project/limit | `SELECT name FROM files WHERE size > 1000 LIMIT 10` |
| **G4** | H+14–18 | `files` source + cache | the disk-usage query; second query is instant |
| **G5** | H+18–26 | **`sqlite()`** | oracle test green against Python. *The headline feature — deliberately not first, because it is the one already spiked* |
| **G6** | H+26–32 | group by, aggregates, order by | the top-5-by-size query |
| **G7** | H+32–36 | csv, tail, cancellation | `tail()` streams; **Ctrl-C stops it** |
| **G8** | H+36–40 | dashboard | live query log over SSE |
| **G9** | H+40–42 | README, STDLIB.md, reproducible build check, video | — |

`join` is slotted opportunistically into G6 if that gate closes early. `json()` is the
declared sacrifice.

---

## 11. STDLIB.md substitutions — 13, bar is 10

| # | Would have used | Written by hand |
| --- | --- | --- |
| 1 | `rusqlite` / `sqlx` | SQLite B-tree reader, varints, serial types, overflow chains |
| 2 | `tokio-postgres` / `postgres` | Postgres v3 wire protocol |
| 3 | `sqlparser-rs` | SQL lexer + Pratt parser |
| 4 | `csv` | CSV parser: quotes, embedded newlines, escapes |
| 5 | `serde_json` | JSON parser *(cut candidate)* |
| 6 | `clap` | argument parsing + `--help` |
| 7 | `walkdir` | recursive `fs::read_dir` |
| 8 | `tokio` / `async-std` | `std::thread` + blocking IO |
| 9 | `anyhow` / `thiserror` | `ZqlError` + SQLSTATE + `Display` |
| 10 | `chrono` / `time` | unix-epoch → civil date (days-from-civil algorithm) |
| 11 | `hyper` / `tiny_http` | HTTP/1.1 for the dashboard |
| 12 | `crossbeam` | `Arc<Mutex<_>>` + `mpsc` |
| 13 | `rayon` / `itertools` | plain loops; manual thread handling |

Package Killer bonus targets the same list.

---

## 12. Open decisions

- [ ] **WAL handling:** detect-and-refuse (~20 lines) versus read the `-wal` sidecar
      (~150). Refusing is honest and cheap; reading is more impressive. **Default:
      detect-and-refuse, upgrade only if G5 closes early.**
- [ ] **CSV type sniffing:** all-Text (simple, ugly output) versus sniff Int/Real/Text over
      the first 100 rows (~60 lines). Lean sniff — typed columns make the join demo read
      properly.
- [ ] **`--explain`**: printing the plan tree is ~40 lines and is a strong Code Quality
      signal. Slot into G6 if time allows.
- [ ] Fake PID and secret key generation — no RNG in std. Use `SystemTime` nanos mixed with
      the connection counter. Not security-relevant; document that plainly rather than
      pretending otherwise.
