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

## 5. Tracks — pick exactly one

| Track | | zql |
| --- | --- | --- |
| A | Developer Tools & CLI | plausible |
| B | Parsers & Data Formats | plausible |
| **D** | **Data & Storage** | ✅ **the pick** |
| C / E / F | Web & Network / Security & Crypto / Wildcard | no |

**Track D.** zql is a query engine over data files — SQLite B-trees, CSV, the filesystem.
Track B would undersell it as a parser exercise; Track A would undersell the format work
that carries the 30% criterion. Track F would require a rationale in the README for no gain.

*(This was not recorded in earlier planning at all — the entry form requires a track.)*

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
