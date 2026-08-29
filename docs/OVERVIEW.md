# zql — everything

> The complete standalone record. Consolidates the scattered notes in
> `IDEAS-ROUND-2.md`, `SPIKE-PG.md` and `VERDICT.md` into one document.
>
> Status as of 2026-08-17: **live candidate, user-preferred.** Spiked and passed.

---

## The pitch

**One sentence:** *"Open any `.db` file and query it with SQL — your browser history, your
app data, your phone backup. Plus your files and CSVs. Nothing to install."*

> Revised 2026-08-17 after the SQLite spike passed. The previous pitch led with the
> filesystem, which describes a mechanism rather than a felt problem. Leading with "the
> `.db` file you already have and cannot open" scores **4.8** on the pitch instrument,
> up from 4.4, for the same project. See `IDEAS-ROUND-4.md`.

**Thirty seconds:** Every time you want to answer a question about your own machine you
reach for a different tool. `find` plus `du` plus `awk` for disk usage. A throwaway script
for the CSV. `git log` piped through three things. You already know a language that
expresses all of these — SQL — but SQL means a database, and a database means loading
your data in first, which is why nobody bothers. zql skips the loading step. Your
filesystem *is* a table. Point it at a folder and query it. And because it speaks the
PostgreSQL wire protocol, you don't install a client either — `psql` already works.

**Why it isn't a buzzword pitch:** there is no word in it that needs defining for this
audience, and the listener knows within five seconds whether they want it. That is the
test. (See `pitch clarity` in `IDEAS-ROUND-3.md`.)

---

## What it looks like

```
$ zql ~/projects
zql 0.1 — listening on 127.0.0.1:5432
  tables: files, dirs, env   functions: csv(), json(), git(), tail()

$ psql -h 127.0.0.1 -U you
psql (16.2)
Type "help" for help.

you=> SELECT ext, count(*) AS n, sum(size)/1048576 AS mb
you->   FROM files GROUP BY ext ORDER BY mb DESC LIMIT 5;
   ext   |   n   |  mb
---------+-------+------
 .mp4    |    31 | 8842
 .zip    |   204 | 1130
 .jpg    | 12904 |  921
 .pdf    |   688 |  402
 .rs     |  3311 |   14
(5 rows)
```

The prompt is PostgreSQL's own client. Thirty years of other people's code, completing a
handshake with a Rust binary that has an empty `[dependencies]`.

---

## Why this shape

Three of the four rubric criteria are things a judge must **take your word for**. They
cannot re-derive that your decoder is correct or your code is idiomatic in the four
minutes they spend on your entry. So they use a proxy. The strongest proxy available is:

> **Does a famous program, written by other people, successfully talk to your binary?**

That is proof a judge cannot argue with and does not have to trust. It also hands you free
demo material, because somebody else already built the UI.

Second filter, from `PRIOR-ART.md`: **the judge pool is backend infrastructure
engineers** — Cisco, Reddit, DoorDash, Okta, Red Hat, Intuit, and one who spent eight
years scaling MySQL infrastructure at Meta. A hand-written Postgres v3 wire
implementation is not a curiosity to that person. It is catnip.

---

## The spike — what was actually proven

380 lines. Empty `[dependencies]`. Compiled with **zero warnings**. Five tests, five
passes.

| # | Test | Result |
|---|---|---|
| 1 | Real `psql` 16.2 connects and renders result sets | **PASS** — its own column alignment, its own row counts, driven by a hand-written `RowDescription` |
| 2 | `node-postgres` (independent JS implementation) connects | **PASS** — read type OIDs correctly: `name:25, ext:25, size:20, dir:25`; clean shutdown |
| 3 | SSLRequest handling | **PASS** — `sslmode=require` correctly refused, `sslmode=prefer` connects |
| 4 | Errors surface as real Postgres errors | **PASS** — node-postgres parsed `severity: 'ERROR', code: '0A000'` |
| 5 | Scale | **PASS** — 127,490 rows in 4.9–6.6 s with the filesystem walk redone *every query*, no index, no cache. Filtered+sorted: 841 ms |

**Two independent client implementations agreeing is much stronger evidence than one.**
`psql` is C and libpq; node-postgres is JavaScript written from the spec. They share no
code. Both accepted the same bytes.

Budgeted 600 lines for the wire module; the subset actually needed came in around 200.

---

## Protocol reference (verified, not guessed)

Everything below was confirmed live against `psql` 16.2 and node-postgres.

### Framing

Every message except the startup packet is: **one tag byte**, then **Int32 length**, then
the body. The length **includes the four length bytes but not the tag**. Getting that
off by one is the classic first bug.

```rust
fn send(&self, w: &mut impl Write) -> io::Result<()> {
    w.write_all(&[self.tag])?;
    w.write_all(&((self.buf.len() + 4) as i32).to_be_bytes())?;
    w.write_all(&self.buf)
}
```

### The startup dance

The first packet has **no tag byte** — just Int32 length, then Int32 code. Loop until you
get a real StartupMessage, because a client may send several of these first:

| Code | Meaning | Correct reply |
|---|---|---|
| `80877103` | SSLRequest | **single bare byte `N`** — not a tagged message |
| `80877104` | GSSENCRequest | same, single `N` |
| `80877102` | CancelRequest | **set the cancel flag for that PID** — see `ARCHITECTURE.md` §4.5. (This line previously read "close the connection"; that was wrong and would have broken the `tail()` demo.) |
| `196608` | StartupMessage (v3.0) | read the param pairs, proceed |

```rust
match code {
    SSL_REQUEST | GSSENC_REQUEST => {
        // Single byte, NOT a tagged message. 'N' = not supported.
        // Getting this wrong looks like a hang, not an error.
        w.write_all(b"N")?; w.flush()?;
    }
    CANCEL_REQUEST => return Ok(()),
    PROTO_V3 => { let body = read_n(&mut r, len - 8)?; params = parse_startup_params(&body); break; }
    _ => return Err(...),
}
```

Then send, in order: `AuthenticationOk` (`'R'` + Int32 0 — **trust auth needs no
crypto**), a run of `ParameterStatus` (`'S'`), `BackendKeyData` (`'K'`), and
`ReadyForQuery` (`'Z'` + `'I'`).

The eleven ParameterStatus pairs that keep clients happy: `server_version` (say `16.2`),
`server_encoding`, `client_encoding` (`UTF8`), `application_name`, `is_superuser`,
`session_authorization`, `DateStyle`, `IntervalStyle`, `TimeZone`,
`integer_datetimes`, `standard_conforming_strings` (`on`).

### Query cycle

| Tag | Message | Direction |
|---|---|---|
| `'Q'` | Simple query | client → us |
| `'T'` | RowDescription | us → client |
| `'D'` | DataRow | us → client |
| `'C'` | CommandComplete | us → client |
| `'E'` | ErrorResponse | us → client |
| `'Z'` | ReadyForQuery | us → client |
| `'X'` | Terminate | client → us |

Type OIDs that matter: `text=25`, `int8=20`, `int4=23`, `bool=16`, `float8=701`,
`timestamp=1114`.

**All values may go over the wire as text.** The binary format is optional. This is a
large saving and it was confirmed working.

### The traps, in order of how much time they can eat

1. **SSLRequest.** Miss it and there is no error — the connection just hangs. Confirmed
   experimentally. Build this first, test it first.
2. **Length field semantics.** Includes itself, excludes the tag.
3. **ErrorResponse is a field-map, not a string.** Key byte then NUL-terminated value,
   terminated by a zero byte. `S` severity, `V` severity-nonlocalized, `C` SQLSTATE,
   `M` message. Clients that get a malformed one report nothing useful.
4. **Extended query protocol** (`Parse`/`Bind`/`Describe`/`Execute`/`Sync`) is what GUI
   clients use. Confirmed out of scope — the spike rejects it and node-postgres reported
   the rejection cleanly. `psql` is the guaranteed client; **DBeaver/TablePlus are a
   stretch goal, never a promise.**

---

## Scope

### Tables and sources

| Source | Feasible in pure std? | Notes |
|---|---|---|
| `files` / `dirs` | ✅ | `fs::read_dir` recursive walk, `metadata()` |
| `env` | ✅ | trivial |
| `csv('path')` | ✅ | hand-written parser: quotes, embedded newlines, escapes |
| `json('path')` | ✅ | hand-written parser, arrays of objects → rows |
| **`sqlite('path')`** | ✅ **SPIKED, PASSED** | **the headline source.** B-tree walk, varints, serial types, overflow chains. Verified byte-exact against Python's `sqlite3` on 8 KB pages, WAL, Unicode table names, `i64::MIN`/`MAX`, and a 30,000-byte overflow value. See `IDEAS-ROUND-4.md`. |
| ~~`git()`~~ | ✅ but **cut** | reads `.git` objects directly; needs zlib INFLATE. **Replaced by `sqlite()`** — 500 lines, strictly less useful and less impressive |
| `tail('path')` | ✅ | streaming source, query never finishes |
| **processes** | ❌ | **std exposes no process enumeration on Windows.** Shelling out to `tasklist` is a disclosure I would rather not make. **Cut, and say so.** |
| network sockets | ❌ | same reason |

### SQL subset — **freeze this on day one, in writing**

The parser is not hard. It is **unbounded** — every feature suggests two more. The subset
*is* the schedule.

**In:** `SELECT` · expressions with the usual operators · `FROM` (one table or table
function) · `WHERE` · `GROUP BY` · `HAVING` · `ORDER BY` (multi-key, `ASC`/`DESC`) ·
`LIMIT`/`OFFSET` · aggregates `count sum avg min max` · scalar functions
`lower upper length substr replace round coalesce` · `LIKE` · `IN` · `BETWEEN` ·
`IS NULL` · `CASE WHEN` · aliases · `\d`-style introspection.

**Out, stated in the README so it reads as a decision rather than a gap:** subqueries,
CTEs, window functions, correlated joins, `UPDATE`/`INSERT`/`DELETE`, transactions,
prepared statements.

**`JOIN` is the swing item.** In if day two goes well, cut without regret if it doesn't.

### Module costing

| Module | Lines | Cuttable? |
|---|---|---|
| Postgres v3 wire protocol | 400 | no — the whole point |
| SQL lexer + Pratt parser | 800 | no |
| Planner + volcano executor | 600 | no |
| Type system, values, coercion | 300 | no |
| Table sources: filesystem, env | 350 | no |
| CSV + JSON table functions | 450 | partially |
| External sort + hash aggregate | 250 | no |
| `JOIN` | 300 | **yes** |
| **`sqlite()` source** | **600** | **no — the headline feature** |
| Dashboard: HTTP + SSE | 400 | **protect it** — worth demo 4.0 → 4.6 |
| CLI + `--help` | 150 | no |
| **Full** | **4,600** | |
| **Floor (JOIN + dashboard cut)** | **3,900** | |

That ladder structure is deliberate: three independently cuttable modules at the end
means the project has a shippable state at every hour after roughly H+30.

---

## Scoring

Post-audit numbers from `VERDICT.md`, after both candidates were spiked.

| Criterion | Weight | zql **+sqlite** | zql (before) | darkroom-halved | Note |
|---|---|---|---|---|---|
| Functionality & Usefulness | 35% | **4.6** | 4.4 | 4.3 | everyone owns `.db` files they cannot open |
| Zero-Dependency Craft | 30% | **4.8** | 4.6 | 4.8 | B-trees + overflow chains, now verified |
| Code Quality & Idiom | 25% | 4.5 | 4.5 | **4.6** | both ~4,500 lines, both decompose well |
| Innovation | 10% | **4.8** | 4.8 | 4.1 | not close — "surprising" is the literal wording |
| **Composite** | | **4.655** | 4.525 | 4.505 | **+0.13 from one source swap** |

### Bonuses — 11/16

| Bonus | Claim |
|---|---|
| Reproducible Build +5 | ✅ verified byte-identical, same toolchain recipe |
| Package Killer +3 | ✅ rusqlite, sqlx, diesel, tokio-postgres, postgres, csv, serde_json, duckdb, walkdir, clap |
| STDLIB Log +3 | ✅ ~11 substitutions, meets the ≥10 bar |
| Single File +5 | ❌ **declined** — 4,000 lines in one file costs more on Code Quality (25%) than the bonus is worth |

### Demo — 4.0 before `sqlite()`, ~4.5 after

**New cold-open:** open a real `.db` file live and query it. No setup, no explanation —
the judge has one of those files too, and has been unable to open it. That is stronger
than any SELECT over a filesystem and it lands in under ten seconds.



`psql` connecting is a genuinely powerful moment, but it lands only for judges who know
what a wire handshake costs. Estimate 25 of 30 get it; those 25 are more impressed than
darkroom's 30, but 5 are lost entirely — and past margins were 0.16 across four places.

**Two cheap fixes, ~250 lines each, that buy motion:**

- **`--dashboard`** — browser view on :8080. Live query log, per-query timing,
  rows-scanned counter, streamed over server-sent events.
- **Streaming sources** — `SELECT * FROM tail('app.log') WHERE level='ERROR'` returns rows
  as they are written. The query never finishes; the screen visibly fills.

The prior-art corpus says static projects lose. Both fixes are on the cut ladder, which
means they are also the first things at risk. Protect the dashboard.

---

## Honest limitations

1. **Terminal-first.** The dashboard mitigates but does not make it a visual project the
   way darkroom is.
2. **"Another SQL engine"** is a fair criticism and someone will make it. The answer is
   the empty manifest and the wire protocol, not conceptual novelty.
3. **Parser scope is unbounded.** The single largest schedule risk. Freeze the subset.
4. **GUI clients need the extended protocol.** Confirmed unavailable in the current
   design. Never promise DBeaver.
5. **No process or network tables.** std does not expose them on Windows.
6. **No project-level preflight.** Environment work (reproducible build, toolchain, LAN,
   QR) carries over from darkroom. Test corpus, fixtures and schedule do not — they are
   darkroom's and would need rebuilding.
7. **Read-only.** No writes, no transactions. Correct scoping, but it will be noticed.

---

## Prior art, stated plainly

DuckDB, osquery, `q`, `textql`, `lnav`, `trdsql`. All exist, several are loved. **This is
the familiar-category-under-hard-constraint shape that every winner in the corpus has**,
and it is a feature, not a problem: `PRIOR-ART.md` finding (3) is that *nobody has ever
won these events on an original concept*.

What is not done: any of them with an empty dependency manifest, speaking the Postgres
wire protocol.

---

## Open items if zql is chosen

- [ ] Freeze the SQL subset in writing, **day one, before any parser code**
- [ ] Write zql's own `SCHEDULE-72H.md` around the G0-G9 gates *(the existing one is
      darkroom's and now lives in `../../Darkroom/docs/`)*
- [ ] Write zql's own `DEMO-SCRIPT.md`: cold-open on `psql` connecting, not on a query
      *(darkroom's is in `../../Darkroom/docs/`)*
- [ ] Decide dashboard in/out **by H+30**, not later
- [ ] Keep `psql` at `D:\Aniket\rust\tmp\pg\pgsql\bin\psql.exe` as the conformance oracle
- [ ] Keep node-postgres in scratchpad as the *second* independent oracle
- [ ] Build and test SSLRequest handling **first**, before anything else
- [ ] Delete `scratchpad/pgspike/` before kickoff — it must never be committed
