# The hackathon — rules, rubric, deadlines

> **Zero Dependency Hackathon** · zerodepshack.com · organised by Hackathon Raptors
> Everything on this page is taken from the event site and re-checked. Where something is
> my inference rather than published text, it says so.

---

## 1. The one rule

> **The shipped artifact's dependency manifest ships empty.** Standard library only.

For this project that means `Cargo.toml` has an empty `[dependencies]` section, and
`cargo tree` shows exactly one node.

**What counts as a dependency:**

| | |
| --- | --- |
| Third-party crates | ❌ forbidden — this is the whole rule |
| Vendored source copied into the repo | ⚠️ **penalized** unless disclosed in `STDLIB.md` |
| The Rust standard library | ✅ that is the point |
| `extern "system"` calls into `kernel32` / Win32 | ✅ platform ABI, not a package — but declare it, and note it is `unsafe` and Windows-only |
| Compilers, build tools, formatters, stdlib test tools | ✅ explicitly do not count against you |
| A dev-only test dependency | allowed **only** where the language ships no test framework, and must be disclosed in `STDLIB.md`. **Rust ships `#[test]`, so this does not apply to us — `Cargo.toml` shows nothing at all** |
| Test *data* (fixture files) | ✅ data is not a dependency |

---

## 2. Judging rubric

| Weight | Criterion | Published wording |
| --- | --- | --- |
| **35%** | Functionality & Usefulness | *"Builds in one command, runs, and matters to a real user"* |
| **30%** | Zero-Dependency Craft | *"Quality of stdlib substitutions; STDLIB.md depth; vendoring penalized"* |
| **25%** | Code Quality & Idiom | *"Reads idiomatic to a senior reviewer in your language"* |
| **10%** | Innovation | *"Surprising stdlib-only builds"* |

Three things follow from the exact wording, and they drove most of zql's design:

1. **"Builds in one command"** is in the *highest-weighted* criterion. `cargo build
   --release` must work on a judge's machine with nothing else installed. No build script
   that needs Python, no fixture generation step.
2. **"matters to a real user"** is why the pitch matters as much as the code. See §5.
3. **"reads idiomatic to a senior reviewer"** at 25% is why the Single File bonus was
   declined — see §3.

**Score calibration from prior events:** winners land **4.08–4.48**; the highest published
score anywhere is 4.88. Margins are thin — 0.162 separated four places at Vanilla Web
Warriors, 0.150 separated three at System Collapse. A 0.13 improvement is not noise.

---

## 3. Bonuses — ⚠️ "Pick one and do it well"

| Bonus | Difficulty | Points | Decision |
| --- | --- | --- | --- |
| **Reproducible Build** | Hard | +5 | ✅ **This is the one.** Already verified on this machine — two clean builds, identical SHA-256. See `BUILD.md`. |
| **Single File** | Hard | +5 | ❌ **Declined.** 5,000+ lines in one file is a stunt that costs more inside the 25% Code Quality criterion than the +5 buys, and it opposes the instruction to maximise implementation. |
| **Package Killer** | Medium | +3 | ➖ Not the declared bonus, but **it has its own $100 prize**, and the list gets written regardless: `rusqlite`, `sqlx`, `diesel`, `tokio-postgres`, `sqlparser`, `csv`, `serde_json`, `walkdir`, `clap`, `tokio`. |
| **STDLIB Log** | Medium | +3 | ➖ Not the declared bonus — **`STDLIB.md` is a required deliverable anyway** and feeds the 30% criterion directly. |

**Correction, 2026-08-17.** Earlier planning assumed all four bonuses stack and budgeted
"11 of 16". The site says **"Pick one and do it well."** So:

- **Declare Reproducible Build.** Highest value, already proven, lowest remaining effort.
- `STDLIB.md` and the Package Killer list still get written — the first is mandatory, the
  second is a separate prize. They just are not the declared bonus.
- Practical effect on the plan: none. Effect on expectations: the bonus is **+5, not +11**.
  Everything now rests on the four weighted criteria, which is where it belonged anyway.

---

## 4. Timeline

| Event | Date (UTC) | IST |
| --- | --- | --- |
| Registration opens | 2026-07-31 | — |
| Team formation | 2026-08-24 | — |
| Cheat-sheets & track guidance posted | **2026-08-26** | — *(read these — free information)* |
| **Kickoff** | Fri **2026-08-28 18:00** | **23:30** |
| **Code freeze** | Mon **2026-08-31 18:00** | **23:30** |
| Judging window | 2026-08-31 → 09-10 | — |
| Write-up side quest closes | 2026-09-08 | — |
| Winners announced | 2026-09-11 | — |

**Prizes — $1,800 total.** 1st **$800** · 2nd $400 · 3rd $200 · **Package Killer $100** ·
Write-Up side quest $300 across three $100 awards, *judged on insight rather than audience
size*.

**Teams are 1–4; solo is welcomed but 2–3 is the site's recommendation.** We are solo — a
known handicap on a 35%-weighted "does it do enough" criterion, and part of why the cut
order in `FEATURES.md` §8 is decided in advance.

72 hours on paper. **~42 realistic building hours** after sleep, meals, and the fact that
kickoff lands at 23:30 local.

The write-up side quest is a separate $300 prize pool across three places, judged on the
build write-up rather than the code. `JOURNAL.md` exists to feed it — append from hour one.

---

## 4b. The published cheat-sheets, checked against zql

Posted 2026-08-26 at `zerodepshack.com/cheatsheets/`. Read on 2026-08-30 and audited
against the code rather than skimmed. Three things came out of it.

**It overturned the track.** See §5 — the biggest single item, and the reason reading
these was worth the hour.

**Two rulings that settle questions we did not have to ask.** "Bun and Deno built-ins
count" and "`node:sqlite` is a Release Candidate, not an experiment" are both about
JavaScript runtimes, so neither touches us. Worth knowing they exist: the pattern is that
runtime APIs are inside the line and the reasoning belongs in `STDLIB.md`.

**Its Rust section matches what we built, which is a useful negative result.** Every gap
it names is one zql had to fill:

| The cheat-sheet says Rust `std` has no… | zql |
| --- | --- |
| async runtime — *"threads plus `std::sync::mpsc` are your answer"* | `std::thread` per connection, blocking IO |
| JSON | hand-written escaping in `dash::sse` |
| HTTP client or server — *"`TcpListener` plus a hand-rolled HTTP/1.1 parser is the realistic path… plan for it on day one"* | `dash::mod` — and it was day one |
| `rand` | the backend secret is `SystemTime` nanos mixed with a counter, said plainly in the README |
| date formatting | `datetime.rs`, days-from-civil |
| regex | the `LIKE` matcher, backtracking rather than recursive |

Its "instead of installing it" table for Rust lists six crates. zql uses **none** of them,
and already replaces the two that matter here — `tokio` and `clap`. `itoa`, `once_cell`,
`fs2` and `crossbeam-channel` never came up.

**One thing it named that we could still take.** `format_into` with `core::fmt::NumBuffer`
landed in **Rust 1.98** and is called *"the cleanest kill on this list"* for replacing
`itoa`. zql renders every integer on the wire with `to_string()`, which allocates per
value; `NumBuffer` would not. It is a real improvement and it is **not taken**, for two
reasons: the toolchain is pinned to 1.97.1 and every piece of reproducible-build evidence
was produced against it, and Package Killer is not the declared bonus. Recorded here so
the decision is visible rather than looking like an oversight.

*(Also from the cheat-sheet: `<[T]>::as_chunks`, stabilised in 1.88, which zql already
uses in the UTF-16 decoder — arrived there by way of a clippy lint rather than this page.)*

---

## 5. Tracks — pick exactly one

| Track | | zql |
| --- | --- | --- |
| A | Developer Tools & CLI | plausible |
| **B** | **Parsers & Data Formats** | ✅ **the pick** |
| D | Data & Storage | ~~the earlier pick~~ — **overturned, see below** |
| C / E / F | Web & Network / Security & Crypto / Wildcard | no |

**Track B — decided 2026-08-30, overturning the 2026-08-17 choice of Track D.**

The original reasoning was that Track B would "undersell it as a parser exercise". That
was a guess about how the track is graded, made before the cheat-sheets were published on
2026-08-26. The published guidance says otherwise, and it is specific enough to settle it.

**Track D's grade is durability.** In its own words: *"fsync after append, or say plainly
in the README that you did not"*, and *"a log-structured store plus an in-memory index is
the shape that fits in 72 hours and survives a restart"*. **zql is read-only and never
opens a file for writing** — there are zero write calls in `src/`. It cannot score on any
of that, because there is nothing to make durable. The only Track D criterion it meets is
the negative one: it does not wrap `sqlite3` and call that a storage engine.

**Track B's grade is what zql spent its 72 hours on.** Point by point:

| Track B says | zql |
| --- | --- |
| *"keep a byte offset and a line/column counter from the first character"* | every `Token` carries a 1-based position; it is what draws `psql`'s caret, and it is also how the DDL parser recovers a column's original case |
| *"retrofitting this on day three is miserable"* | it was in `token.rs` from the first commit |
| *"table-driven tests over a corpus of ugly inputs"* | 295 tests, including every canonical query truncated at every byte and mangled at every position |
| *"target a suite judges can run"* | Python's `sqlite3` wrote the fixtures and supplies every expected value; `psql` and node-postgres check the protocol |
| *sinks it: "a parser that only handles the happy path"* | 62 deliberately corrupted databases, all producing errors and no panics |

zql is, structurally, **five parsers**: SQL (lexer and Pratt parser), the SQLite file
format (b-tree, varints, serial types, overflow chains), `CREATE TABLE` DDL, RFC 4180 CSV,
and the PostgreSQL v3 wire protocol. That is the track.

**The counter-argument, for the record:** a judge may read "Parsers & Data Formats" as
underselling a working query engine with a network server attached. That is a real cost.
It is the smaller one — the parsers are where the 30% Craft criterion is won, and the
track should point at them rather than at a durability story that does not exist.

---

## 6. Deliverables

- **Public GitHub repo** with an **OSI-approved license**
- **One-command build** producing a runnable artifact
- **Empty dependency manifest**
- **Dependency proof** — command output or a CI log. *(Not previously recorded: budget
  ~20 minutes to capture `cargo tree` output or a CI run.)*
- **`README.md`** — what it does, how to run, and its limits
- **`STDLIB.md`** — each package-for-stdlib substitution
- **A 5-minute demo video** showing **the tool** *and* **the empty manifest**. The manifest
  shot is required, not a flourish. The most common way to lose points is forgetting it.
- Be reachable for judge follow-up

**Explicitly out of scope**, per the site — worth checking zql against each: trivial toys;
empty manifests that shell out to separately installed tools; undisclosed vendoring;
homemade ciphers; LLM dumps with no docs; anything needing custom hardware, GUI frameworks,
or a running third-party service. **zql clears all seven** — note especially that it does
not shell out to `sqlite3` and does not need a running Postgres.

**On AI:** allowed and expected, explicitly not penalised. Judges evaluate whether the
artifact holds up, and the docs are the receipts.

---

## 7. Structural constraints discovered, that the rules imply but do not state

- **No TLS, therefore no real internet.** X25519 + ChaCha20-Poly1305 + SHA-256 + HKDF +
  X.509 is ~1,500 lines *without* certificate validation, and shipping unvalidated TLS is
  security theatre. This eliminates any feature whose value comes from reaching an external
  service. zql is entirely local and plaintext-LAN, and that is a consequence of this
  constraint, not a coincidence.
- **No RNG in std.** Anywhere randomness is needed (the Postgres backend secret key), it
  comes from `SystemTime` nanos mixed with a counter, and the README says so plainly rather
  than implying security it does not have.
- **No system time zone database.** All timestamps are UTC, stated in the README.
