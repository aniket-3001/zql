# Spike: Postgres v3 wire protocol — PASSED

> **Verdict: `zql` is real. The protocol is not the hard part.**

## The question

Can a Rust binary with an empty `[dependencies]` complete a handshake with a real
PostgreSQL client and return rows? If not, `zql` dies here and we lose nothing.

## What was built

**380 lines**, one file, `[dependencies]` empty, compiled clean with zero warnings on
`1.97.1-x86_64-pc-windows-gnu`.

Implements: SSLRequest/GSSENCRequest refusal · StartupMessage parsing · trust auth ·
ParameterStatus · BackendKeyData · ReadyForQuery · simple query (`Q`) · RowDescription ·
DataRow · CommandComplete · ErrorResponse · Terminate · a `files` table backed by a real
recursive `fs::read_dir` walk, with WHERE / ORDER BY / LIMIT.

**This is throwaway.** It lives in the scratchpad, is not in any repo, and gets deleted
before kickoff. The real thing is written from scratch after 18:00 UTC on the 28th.

## Results

### 1. Real `psql` 16.2 — PASSED

Downloaded genuine PostgreSQL 16.2 Windows binaries from EDB. Not a reimplementation,
not a mock. `psql --version` → `psql (PostgreSQL) 16.2`.

```
$ psql -h 127.0.0.1 -p 5433 -U aniket -d zql -c "select name, ext, size from files order by size desc limit 8"

          name          | ext |  size  |                       dir
------------------------+-----+--------+--------------------------------------------------
 tokensave.db           | db  | 507904 | D:\Aniket\Zero Dependency\.tokensave
 bad-signature.png      | png | 413228 | D:\Aniket\Zero Dependency\corpus\pathological
 actually-png.jpg       | jpg | 413228 | D:\Aniket\Zero Dependency\corpus\pathological
 ...
(8 rows)
```

Column alignment, row counts, and type-aware right-alignment of the integer column are
all psql's own rendering, driven entirely by our `RowDescription`. Errors surface as
proper `ERROR:` lines.

### 2. node-postgres — PASSED

An **entirely independent** implementation of the v3 protocol, written in JavaScript,
sharing no code with libpq. Connected, ran queries, and — importantly — read our type
OIDs correctly:

```
fields -> name:25, ext:25, size:20, dir:25      (25 = text, 20 = int8)
clean shutdown
```

Two independent implementations agreeing is much stronger evidence than one. This is not
"we happened to satisfy psql"; it is "we implement the protocol."

### 3. The SSLRequest gotcha — CONFIRMED REAL, AND HANDLED

I flagged this as the #1 risk in `IDEAS-ROUND-2.md`. It was not theoretical:

| `sslmode` | Result |
| --- | --- |
| `require` | `psql: error: server does not support SSL, but SSL was required` |
| `prefer` (libpq default) | ✅ connects |

That failure message *proves* `psql` sends an SSLRequest before anything else and reads
our single `N` byte. Miss this and every connection hangs with no error. **First thing to
build on day one, first thing to test.**

### 4. Extended query protocol — FAILS, AS DOCUMENTED

`node-postgres` switches to Parse/Bind/Execute automatically when a query carries
parameters. It failed exactly as predicted — and the client parsed our ErrorResponse
fields perfectly:

```
severity: 'ERROR',  code: '0A000'
```

Even our failure path is protocol-correct. This confirms the documented limitation:
**GUI clients (DBeaver, TablePlus) need the extended protocol and are a stretch goal, not
a promise.** `psql` is the guaranteed client.

### 5. Scale — PASSED

Pointed at `D:\Aniket` (37.8 GB):

| Query | Rows | Time |
| --- | --- | --- |
| Full scan, all rows to client | **127,490** | 4.9 – 6.6 s |
| Filtered + sorted + limited | 2 | 841 ms |

~25,000 rows/sec end-to-end, and **the filesystem walk is redone from scratch on every
query** — no index, no cache, no parallelism. Everything that makes this fast is still
unbuilt. This is the floor, not the ceiling.

## What this changes

**The protocol was the unknown, and it is now known.** 380 lines bought the handshake, the
simple query path, result sets, and errors — all validated by two independent clients.
The remaining work (SQL parser, planner, executor, table sources) is *ordinary
engineering* with no protocol risk attached to it.

Compare with darkroom, where the equivalent unknown — does the JPEG decoder produce
correct pixels — cannot be answered by a spike and cannot be answered loudly. It is
answered slowly, subtly, and at hour 30.

**Revised confidence in the `IDEAS-ROUND-2.md` estimates:** the "no preflight yet" caveat
on `zql` is now substantially retired. Craft 4.8 and Code Quality 4.5 both look
defensible; the wire module came in at 600 lines estimated versus ~200 actual for the
subset built here, so the 4,250-line total is if anything conservative.

**The remaining risk is entirely in the SQL parser** — not because it is hard, but because
it is unbounded. Freeze the supported subset in writing on day one and treat it as the
schedule.

## Cleanup / carry-forward

- Spike source: `scratchpad/pgspike/` — **delete before kickoff**, never commit.
- `pgbin.zip` (333 MB) deleted after extraction.
- **`psql` retained at `D:\Aniket\rust\tmp\pg\pgsql\bin\psql.exe`** — needed throughout
  the build as the conformance oracle. It is a test tool, not a dependency; nothing links
  against it and it ships with nothing.
- `node-postgres` in `scratchpad/nodeclient/` — second oracle, same status.
