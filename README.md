# zql

[![CI](https://github.com/aniket-3001/zql/actions/workflows/ci.yml/badge.svg)](https://github.com/aniket-3001/zql/actions/workflows/ci.yml)
[![Playground](https://github.com/aniket-3001/zql/actions/workflows/pages.yml/badge.svg)](https://github.com/aniket-3001/zql/actions/workflows/pages.yml)

**[Try it in your browser →](https://aniket-3001.github.io/zql/)** — zql's engine
compiled to WebAssembly, reading a real SQLite file. Not a recording.

**Open any `.db` file and query it with SQL. Your browser history, your app
data, your phone backup. Nothing to install.**

Almost every application on your machine keeps its data in SQLite — browsers,
phones, notes apps, photo libraries. You have dozens of `.db` files and no way
to look inside any of them without installing something.

zql opens them. It does the same for your CSVs and your filesystem, so you can
**join across all of them with one query**. And because it speaks the
PostgreSQL wire protocol, you don't install a client either: `psql` already
works.

```console
$ zql ~/projects
zql 0.1.0 — listening on 127.0.0.1:5432
  connect with:  psql -h 127.0.0.1 -p 5432
```

```console
$ psql -h 127.0.0.1
psql (16.2)
Type "help" for help.

you=> SELECT ext, COUNT(*) AS n, SUM(size)/1048576 AS mb
you->   FROM files WHERE NOT is_dir
you->   GROUP BY ext ORDER BY mb DESC LIMIT 5;
 ext  |   n   |  mb
------+-------+------
 mp4  |    31 | 8842
 zip  |   204 | 1130
 jpg  | 12904 |  921
 pdf  |   688 |  402
 rs   |  3311 |   14
(5 rows)
```

That prompt is PostgreSQL's own client — thirty years of other people's code —
completing a handshake with a Rust binary whose `[dependencies]` section is
empty.

---

## The one rule

```console
$ cargo tree
zql v0.1.0
```

One node. No third-party crates, no vendored source, no FFI, and no `unsafe`.
Everything is the Rust standard library. See **[STDLIB.md](STDLIB.md)** for the
seventeen substitutions and what each one got wrong the first time, and
[`deps-proof.txt`](deps-proof.txt) for the captured output.

## Build and run

```console
cargo build --release
./target/release/zql <directory>
```

That is the whole build. Nothing to generate, no build script, no other
toolchain. Then point any Postgres client at `127.0.0.1:5432`.

```
USAGE:
    zql [DIRECTORY] [OPTIONS]

OPTIONS:
    -H, --host <HOST>          Address to bind [default: 127.0.0.1]
    -p, --port <PORT>          Port to listen on [default: 5432]
    -d, --dir <DIR>            Directory the `files` source walks [default: .]
        --no-cache             Re-walk the filesystem on every query
        --dashboard            Serve a live query log over HTTP [port 8080]
        --dashboard-port <N>   Port for the dashboard
    -h, --help                 Print help
    -V, --version              Print version
```

## What you can query

`SHOW SOURCES` lists them at runtime:

| Source | Signature | Reads |
|---|---|---|
| `sqlite` | `sqlite('file.db', 'table')` | a table in a SQLite database |
| `files` | `files` or `files('path')` | the filesystem, walked recursively |
| `csv` | `csv('file.csv')` | a CSV file with a header row |
| `tail` | `tail('file.log')` | a log file, streamed as it grows |
| `env` | `env` | environment variables of the server process |

Forget which tables a database has? Leave the name out and it tells you:

```console
you=> SELECT * FROM sqlite('places.sqlite');
ERROR:  sqlite() needs a table name
DETAIL:  places.sqlite contains: moz_places, moz_bookmarks, moz_origins, …
HINT:  for example: sqlite('file.db', 'table')
```

### Things worth trying

```sql
-- The cold open: a file you already have and cannot open.
SELECT title, url FROM sqlite('places.sqlite', 'moz_places')
ORDER BY visit_count DESC LIMIT 10;

-- What the tool is actually for: one query language over unlike things.
SELECT f.name, f.size, m.owner
FROM files('src') AS f
JOIN csv('owners.csv') AS m ON f.name = m.filename
WHERE f.ext = 'rs' AND f.size > 1000
ORDER BY f.size DESC;

-- Where the disk went.
SELECT ext, COUNT(*) AS n, SUM(size) AS bytes
FROM files WHERE NOT is_dir
GROUP BY ext HAVING COUNT(*) > 5
ORDER BY bytes DESC LIMIT 10;

-- A live filter over a log. Ctrl-C stops it.
SELECT line FROM tail('server.log') WHERE line LIKE '%ERROR%';

-- What is the plan?
EXPLAIN SELECT ext, COUNT(*) FROM files GROUP BY ext;
```

### The SQL

`SELECT` · `FROM` · `WHERE` · `GROUP BY` · `HAVING` · `ORDER BY` (with
`ASC`/`DESC` and `NULLS FIRST`/`LAST`) · `LIMIT`/`OFFSET` · `DISTINCT` ·
`INNER` and `LEFT JOIN` · aggregates `COUNT SUM AVG MIN MAX` including
`COUNT(DISTINCT …)` · `LIKE` `IN` `BETWEEN` `IS NULL` · `CASE WHEN` · `CAST` ·
thirteen scalar functions · `SHOW SOURCES` · `EXPLAIN`.

The grammar was frozen in writing before any of it was implemented, and the
frozen document is [`docs/SQL-SUBSET.md`](docs/SQL-SUBSET.md).

## The dashboard

`--dashboard` serves a live query log at `http://127.0.0.1:8080` over
server-sent events: every query as it runs, with its timing, row count, and
whether it hit the filesystem index or rebuilt it. The page is one
self-contained HTML document that fetches nothing.

With `--host 0.0.0.0` it also prints a LAN address you can open from a phone.

---

## Limits

Written down because a tool without a limitations section has not been tested.

- **Read-only.** No `INSERT`, `UPDATE`, `DELETE`, or DDL. zql never opens a
  file for writing. This is a product boundary, not an unfinished corner: it
  removes transactions, locking and constraint checking from scope in one
  stroke.
- **Simple query protocol only.** `psql` and `node-postgres` work, because both
  use it by default. GUI clients — DBeaver, TablePlus — negotiate the extended
  protocol (`Parse`/`Bind`/`Execute`) and are refused with a clear `0A000`.
- **No `pg_catalog`, so `psql`'s backslash commands do not work.** `\dt`, `\d`
  and `\l` are not protocol messages: `psql` expands each into a query against
  the Postgres system catalogue, which zql does not have. Each is refused with
  a message saying so and pointing at `SHOW SOURCES`, which is the equivalent.
- **No index use.** Every query is a full scan. Fine at the sizes this is for,
  and the filesystem walk is cached per session, so the second query skips it
  entirely: 23 ms to 1.1 ms over 636 entries, 16.6 s to 6 ms over 251,161. The
  speedup is whatever the walk cost, so it grows with the tree — quote the
  measurement, not a multiplier.
- **Joins are nested-loop**, and the right-hand side is materialised. Two large
  sources joined together will be slow.
- **Expressions nest at most 50 deep**, and deeper ones are refused with
  `54001`. Binding, evaluation and the parser all walk an expression by
  recursion, and a stack overflow — unlike a panic — aborts the process rather
  than the query, so it is the one input that could end the server for every
  connected client. The limit is measured against the tightest case (nested
  function calls in a debug build, which abort at ~110) rather than the roomiest.
  Real SQL nests four levels; a long alternation belongs in `IN (...)`, which is
  flat at any length.
- **Casts are `CAST(x AS type)`, not `x::type`.** The `::` shorthand is refused
  by name with the spelling that works.
- **WAL-mode SQLite files must be checkpointed.** If a database has
  un-checkpointed changes in its `-wal` sidecar, zql **refuses to read it**
  rather than returning the stale rows in the main file. Stale data that looks
  correct is the worst failure available here.
- **Encrypted or extension-modified SQLite files** — anything with non-zero
  reserved page space — are out of scope and refused by name.
- **In-memory sort, grouping and distinct**, with documented row ceilings.
  Above them zql errors with `54000` instead of exhausting the machine.
- **UTC only.** The standard library ships no time-zone database, so every
  timestamp zql reports is UTC.
- **`LIKE` is case-sensitive**, which is Postgres semantics and differs from
  SQLite's default.
- **SQLite column names keep the case they were declared with** — a column
  written `visitCount` is reported as `visitCount`, not folded — but may be
  referred to in any case, as SQLite itself allows. An exact match always wins,
  so this only ever resolves a reference that would otherwise have been an
  error.
- **No process or network tables.** The standard library exposes no process
  enumeration, and shelling out to `tasklist` would be a dependency wearing a
  disguise.
- **`json()` was cut.** `csv()` covers the same ground. It is refused by name
  and says it was a deliberate cut.
- **The backend secret key is not cryptographically random.** There is no RNG
  in the standard library, so it is `SystemTime` nanoseconds mixed with a
  counter. It is guessable. Nothing behind it is sensitive — the worst a
  successful guess achieves is cancelling your own query on a read-only server
  that binds loopback by default.
- **No authentication and no TLS.** zql accepts any user with no password and
  speaks plaintext. Bind it to loopback, which is the default. Do not put it on
  a network you do not control.

### One environment note, not a zql limit

On Windows, `psql -c "SELECT '写真'"` returns `??`. The console converts the
argument to the ANSI codepage before `psql` ever sees it, so the non-ASCII text
is already gone by the time it reaches the wire. The same query typed at the
`psql` prompt, piped in on stdin, or run with `-f` works correctly — zql stores
and returns UTF-8 throughout, including a Unicode table name:

```console
$ echo "SELECT label FROM sqlite('hard.db','写真');" | psql -h 127.0.0.1
 ユーザー
```

## Correctness

zql is checked against independent implementations rather than against itself.

| Layer | Oracle | Result |
|---|---|---|
| Wire protocol | real `psql` 16.2 | 88 acceptance checks |
| Wire protocol | node-postgres — JavaScript, sharing no code with libpq | 19 checks |
| SQLite reader | Python 3's `sqlite3`, which wrote the fixtures | byte-exact |
| Dashboard | an independent HTTP client | 12 checks |
| SQL semantics | golden queries in `tests/` | 295 tests |

Two independent protocol clients agreeing is much stronger evidence than one.
`psql` is C and libpq; node-postgres is JavaScript written from the
specification. Both accept the same bytes — and node-postgres parses results
*by type*, so a wrong OID gives it a wrong JavaScript value rather than a
plausible-looking string.

**295 tests.** Among them: the three-valued logic truth table from the
specification, verbatim; the SQLite reader against oracle values for `i64::MIN`,
`i64::MAX`, π, astral-plane emoji and a 30,000-character overflow chain; and
two adversarial sweeps that assert **zero panics** — every canonical query
truncated at every byte and mangled at every position, and 62 deliberately
corrupted database files.

**And one sweep that asserts something panics cannot cover.** A stack overflow
aborts the process instead of unwinding, so `catch_unwind` cannot contain it and
one connection would take every other one down with it. Twelve deeply-nested
query shapes are therefore driven through a real socket, and the test requires —
as the panic test does — that a bystander session and the listener both survive.

The `catch_unwind` net around each connection is tested rather than asserted:
a source that exists only under `cfg(test)` panics inside a real connection
thread over a real socket, and the test requires that an unrelated session and
the listener both survive it. That test also pins `panic = "abort"` out of the
release profile — under abort the net could not catch anything, and the test
would fail by the process dying.

```console
cargo test
```

On top of those, an acceptance run drives **88 checks through real `psql`** —
connection, every source, the truth table, joins, introspection, and the exact
text of a dozen error messages — and **19 through node-postgres**. The
cancellation path is verified by speaking the protocol directly: a live
`tail()` stream, stopped by a `CancelRequest` on a second connection, which is
precisely what `psql` does on Ctrl-C.

## Reproducible build

Two clean builds of this source produce a byte-identical binary.

```
sha256  8DE8C0DE6526C7AE79837A4FCFA4A3A21259AE1B312A7511C15B69518C28E56D
size    1,814,434 bytes
```

Reproduce with [`build.ps1`](build.ps1). **The envelope, stated honestly:** same
toolchain version (`1.97.1-x86_64-pc-windows-gnu`), same target. This is not a
claim that any machine anywhere produces these bytes — a Linux ELF is not a
Windows PE, and the CI job proves the *property* (determinism) rather than this
particular value.

The build directory does **not** affect the output: a fresh `git clone` into a
temporary path produces the same hash as the original working tree, verified
across three clean builds in two directories. That is stronger than the
"same machine" envelope this section used to claim, and it is why no
`--remap-path-prefix` is needed.

It did not work out of the box. Two clean builds initially differed because the
MinGW linker stamps the current time into the PE header;
`-Wl,--no-insert-timestamp` fixed it. That flag lives in
[`.cargo/config.toml`](.cargo/config.toml), scoped to the windows-gnu target so
that a plain `cargo build --release` still works everywhere else.

## Prior art

DuckDB, osquery, `q`, `textql`, `lnav` and `trdsql` all exist and several are
loved. zql is not a new idea. What has not been done before is any of them with
an empty dependency manifest, speaking the PostgreSQL wire protocol.

## License

MIT. See [LICENSE](LICENSE).
