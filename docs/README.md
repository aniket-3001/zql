# zql — documentation

> **"Open any `.db` file and query it with SQL. Your browser history, your app data, your
> phone backup. Nothing to install."**
>
> A Postgres-compatible server written in Rust with an **empty `[dependencies]`**. Point
> `psql` at it and query SQLite files, CSVs, JSON and your filesystem with one language —
> and join across them.

---

## The documents

Read in this order.

| # | File | What it is |
| --- | --- | --- |
| 1 | **`RULES.md`** | The hackathon: the empty-manifest rule, the rubric and its exact wording, bonuses, deadlines, deliverables, and the pre-kickoff line |
| 2 | **`OVERVIEW.md`** | The idea: pitch, scoring against the rubric, why this shape, prior art, limitations |
| 3 | **`FEATURES.md`** | Everything planned, marked Core / Planned / Stretch, with the binding cut order |
| 4 | **`ARCHITECTURE.md`** | The engineering design: modules, type shapes, interface contracts, algorithm choices, costing, and the hour-by-hour build order |
| 5 | **`SQL-SUBSET.md`** | The frozen grammar and semantics. Anything not in it returns `0A000`, and that is a feature |
| 6 | **`SPIKE-WIRE.md`** | Postgres v3 wire protocol — verified live against `psql` 16.2 |
| 7 | **`SPIKE-SQLITE.md`** | SQLite file format — verified against Python's `sqlite3`, passed first try |
| 8 | **`SPIKE-CANCEL.md`** | Query cancellation — verified against real `psql` with a real Ctrl-C |
| 9 | **`SPIKE-DASHBOARD.md`** | HTTP/1.1 + SSE — verified against Node and a real Chrome `EventSource` |
| 10 | **`BUILD.md`** | Toolchain, the reproducible-build recipe, machine environment, known gotchas |

Still in `../../planning/`: `FINALISTS.md` (zql vs darkroom, the full comparison),
`VERDICT.md`, `IDEAS*.md` (all 21 candidates across four rounds), `PRIOR-ART.md` (what
actually wins Raptors events), `OPTIONS.md`, `PREFLIGHT.md`, `JOURNAL.md`.

darkroom — the runner-up — has its own folder: `../../Darkroom/docs/`. Its
`SCHEDULE-72H.md` moved there with it.

---

## The short version

**What it is.** A server that speaks the PostgreSQL wire protocol, so every Postgres client
already on your machine can talk to it — but instead of a database behind it, there are
your files.

**Why it wins on the rubric.** Deep format work in two places (the wire protocol and the
SQLite B-tree) carries the 30% Craft criterion; a felt, one-sentence problem carries the
35% Functionality criterion; a clean pull-based query engine carries the 25% Code Quality
criterion.

**Where the risk is.** ~5,290 designed lines against ~42 building hours. The cut order is
already decided, in writing, in `FEATURES.md` §8 — so it is a calm decision now instead of
an hour-38 one.

---

## Current status

| | |
| --- | --- |
| Wire protocol | ✅ spiked, verified against `psql` 16.2 |
| SQLite reader | ✅ spiked, verified against Python `sqlite3`, first try |
| Query cancellation | ✅ spiked, verified against real `psql` + real Ctrl-C |
| Dashboard (HTTP + SSE) | ✅ spiked, verified against Node and real Chrome |
| Reproducible build | ✅ verified byte-identical, twice |
| Registration | ✅ confirmed 2026-08-16 |
| Architecture & grammar | ✅ this folder |
| Source code | ⛔ **not until 2026-08-28 18:00 UTC** |

## Open decisions

- [ ] **WAL handling** — refuse loudly (~20 lines) vs read the `-wal` sidecar (~150)
- [ ] **CSV type sniffing** — all-Text vs sniff over the first 100 rows (~60 lines); leaning sniff
- [x] ~~**Write zql's own demo script.**~~ Written, and kept outside the repo — a
      presenter's script is for the presenter, not for a judge reading the source.
- [ ] **Write zql's own `SCHEDULE-72H.md`** around the gates in
      `ARCHITECTURE.md` §10. The existing pair are darkroom's and now live in
      `../../Darkroom/docs/`
- [ ] **Delete the six spikes before kickoff:** `scratchpad/pgspike/`, `jpegspike/`,
      `sqlspike/`, `conspike/`, `cancelspike/`, `ssespike/`
