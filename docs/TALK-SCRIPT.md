# zql — the talk script

> Ten minutes with the slides, at **https://aniket-3001.github.io/zql/**
>
> **There is no separate deck.** Press **Present** on the page — or the button
> in the masthead — and each section becomes a full-screen slide driven by
> **← / →**, space, or page keys. **Esc** exits. One artifact, so the deck
> cannot drift from the site the way a `.pptx` in a drive folder always does.
>
> Nine slides. The live demo is slide 2 — you are presenting *in* the thing,
> which is the whole advantage of this format. For a demo-only run, use
> [`DEMO-SCRIPT.md`](DEMO-SCRIPT.md).

---

## Before you start

- [ ] Load the page, wait for **Run** to enable, *then* press Present.
- [ ] Test **←/→** once before the audience is watching.
- [ ] Zoom to 125–150%.
- [ ] Have a terminal ready for the manifest shot on slide 9.
- [ ] Know how to get out: **Esc**.

---

## Slide 1 · zql
### *the masthead*

> "Almost every application on your machine keeps its data in SQLite. Your
> browser. Your phone backup. Your notes app, your photo library. You have
> dozens of `.db` files sitting on disk right now, and no way to look inside any
> of them without installing something."

> "zql opens them. You query them with SQL. And the entire thing is written
> against the Rust standard library — the dependency manifest is empty."

**Point at the `cargo tree` block on the slide.**

> "One node. That's not a trimmed list, that's the whole tree."

*(≈45 seconds. Don't rush this — the premise is the whole talk.)*

---

## Slide 2 · Run it. This is the real engine.
### *the live playground*

**This is the slide that wins the room. Spend the most time here.**

> "I'll show you rather than tell you. This is zql's engine compiled to
> WebAssembly, running in this browser tab."

**Do:** click **"The cold open — browser history"**.

> "That's a real Firefox history database. The reader is walking its b-tree
> right now — not a recording, not a mock, not a server call. The same code that
> would read it off your disk."

**Do:** click **"GROUP BY over a CASE"**, then expand the plan.

> "Real SQL. `GROUP BY` over a `CASE`, `HAVING`, `ORDER BY` — and that's the
> operator tree the binder produced."

**Do:** click **"A typo"**.

> "And errors are a feature. A real SQLSTATE, a caret at the exact character,
> and a suggestion — `form` is one transposition away from `FROM`, which is the
> typo everybody actually makes."

> "Everything on this page you can type into yourself. It's the shipped engine."

*(≈2 minutes. Resist doing every preset — three is plenty.)*

---

## Slide 3 · The problem

> "So why build this. The data is right there on your disk, in an open,
> documented format, and it may as well be encrypted. You need a client. And the
> moment you want to compare it against a CSV, or against what's actually on
> disk, you need a second tool and a third."

*(≈30 seconds.)*

---

## Slide 4 · The answer — one binary, five sources

> "One query language over five sources. SQLite tables, your filesystem, CSV
> files, a log file streamed as it grows, and environment variables."

> "And they join to each other. That's the feature the whole tool exists for.
> Everything else zql does, something else already does better — but nothing
> else lets you join a SQLite table against a directory listing."

*(≈45 seconds.)*

---

## Slide 5 · Why the wire protocol

> "Here's the decision I'm proudest of. 'Nothing to install' has to include the
> client — otherwise I've just moved the problem."

> "So zql speaks the PostgreSQL wire protocol. Every Postgres client already on
> your machine connects to it. `psql`, `node-postgres`, anything."

**Point at the terminal block.**

> "That's thirty years of somebody else's C completing a handshake with a binary
> whose dependency list is empty."

> "It also gave me an oracle for free. node-postgres parses results *by type* —
> so if I'd got a type OID wrong, it would hand back a wrong JavaScript value
> instead of a plausible-looking string. Two independent clients agreeing is far
> better evidence than one."

*(≈1 minute.)*

---

## Slide 6 · What was written by hand

> "Seventeen substitutions. Every one of these is a crate a working engineer
> would reach for without thinking."

**Point at two or three, not all twelve on screen.**

> "The SQLite file format — the header, the b-tree walk, varints, serial types,
> overflow chains, and parsing `CREATE TABLE` text, because SQLite stores no
> structured column list. The only record of a table's columns is the original
> statement."

> "The Postgres wire protocol. The SQL parser. The async runtime, which turned
> out to be a thread per connection and blocking IO, and that was genuinely
> enough."

> "`STDLIB.md` lists all seventeen with what each one cost. Several of them are
> honestly worse than the crate — the `LIKE` matcher is quadratic in its worst
> case where a regex engine would be linear. A list of seventeen wins and no
> losses would be less believable, not more."

*(≈1 minute.)*

---

## Slide 7 · How it is checked

> "The thing I'd most want a reviewer to look at. Everything is checked against
> *other people's* implementations, not against itself."

> "A reader checked against its own output proves self-consistency — and
> self-consistency is exactly what a misunderstanding of the format also
> produces. So the SQLite fixtures are written by Python's `sqlite3`, and the
> expected values come from `sqlite3`, not from zql."

> "That caught two real bugs. `INTEGER PRIMARY KEY` reads as NULL in every
> record, because the value lives in the cell header rather than in the row. And
> a `REAL` column holding three-seventy-five-point-zero stores an *integer* — so
> a reader that returns it as one disagrees with every other SQLite client about
> what's in the file. Both were plausible enough to survive reading the output.
> Only the oracle caught them."

*(≈1 minute 15. This is the slide that separates the project from a weekend
hack — give it room.)*

---

## Slide 8 · The bug that mattered most

> "And one I want to be honest about, because it nearly shipped."

> "A four-kilobyte query could end the server for every connected client. About
> seventeen hundred `1+1+1`s. Not the connection — the *process*."

> "A stack overflow isn't a panic. It aborts rather than unwinding, so the
> `catch_unwind` I had around every connection could not catch it. I confirmed
> it: a bystander session on a different connection died with it, and the
> listener stopped accepting."

> "It's fixed by bounding expression depth where the tree is built, which closes
> binding, evaluation and the drop glue at once. And the limit is *measured* —
> nested calls abort at about a hundred and ten levels in a debug build against
> seventeen fifty in release, so the cap is sized against the tighter number. My
> first guess was wrong by a factor of ten."

> "I found it by fuzzing the running server, not by reading the code."

*(≈1 minute. Audiences trust the rest of the talk more after this one.)*

---

## Slide 9 · What it does not do → The receipts

**Two slides; move briskly through the limits.**

> "A tool without a limitations section hasn't been tested. Read-only. Simple
> query protocol only, so GUI clients are refused by name. WAL databases are
> refused rather than read stale — returning plausible but week-old rows is
> worse than refusing. UTC only, because the standard library has no time-zone
> database. And the backend secret isn't cryptographically random, because
> `std` has no RNG — that's in the README too."

**Advance to The receipts.**

> "Two hundred and ninety-five tests, passing in both release and debug. Zero
> `unsafe`, zero FFI, zero shelling out. Three platforms green in CI. And a
> reproducible build — two clean builds produce byte-identical output, verified
> on hardware I don't control."

**Do:** Esc out, switch to the terminal, and run it live.

```
$ cargo tree
zql v0.1.0
```

> "One node. Thank you — happy to take questions, and the playground is live if
> anyone wants to try to break it."

*(≈1 minute 15.)*

---

## Timing

| Slide | Target | Running |
|---|---|---|
| 1 · zql | 0:45 | 0:45 |
| 2 · Live demo | 2:00 | 2:45 |
| 3 · The problem | 0:30 | 3:15 |
| 4 · The answer | 0:45 | 4:00 |
| 5 · Wire protocol | 1:00 | 5:00 |
| 6 · Written by hand | 1:00 | 6:00 |
| 7 · How it is checked | 1:15 | 7:15 |
| 8 · The bug | 1:00 | 8:15 |
| 9 · Limits + receipts | 1:15 | 9:30 |

**Cutting to five minutes:** keep 1, 2, 5, 7, 9. Drop the problem statement (the
demo makes it), the substitution list, and the bug.

**Stretching to fifteen:** take questions at slide 2, and open the console on
slide 8 to reproduce the depth limit live.

---

## Questions you should expect

**"Why not just use DuckDB / `q` / osquery?"**
> "You should, for most things — they're in the README under prior art. What
> hasn't been done is any of them with an empty dependency manifest, speaking
> the Postgres wire protocol. That's the interesting constraint, not the SQL."

**"Isn't writing your own SQLite reader reckless?"**
> "For production, yes. This one is read-only, refuses anything with reserved
> page space, refuses un-checkpointed WAL files rather than returning stale
> rows, and is checked against `sqlite3` value by value. It never opens a file
> for writing — there are zero write calls in the source."

**"How long did it take?"**
> "Seventy-two hours, and about a third of that was the evidence rather than the
> code — the oracles, the fixtures, the fuzzing that found the stack overflow."

**"What would you do next?"**
> "Read the WAL sidecar instead of refusing it, and the extended query protocol
> so GUI clients work. Both are scoped in `FEATURES.md` — they were cut on
> purpose, not forgotten."

**"What's the WebAssembly bit — is that part of the project?"**
> "It's the demo, not the product; it lives in `bridge/` and depends on zql
> rather than the other way round. The engine is compiled to WASI so `std::fs`
> keeps working, which is what lets the SQLite reader open a real file in a
> browser tab. The WASI host is hand-written too — the page fetches nothing
> external, same rule as the binary."
