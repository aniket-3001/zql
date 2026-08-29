# zql — the demo script

> Five minutes, live, on the playground at
> **https://aniket-3001.github.io/zql/**
>
> This is the script for *showing the thing working*. The script for talking
> over the slides is [`TALK-SCRIPT.md`](TALK-SCRIPT.md). They share an opening
> so either can be given alone.
>
> **The one rule:** the hackathon requires the video to show the tool **and the
> empty manifest**. Forgetting the manifest shot is the commonest way to lose
> points. It is beat 1 and beat 8 here, deliberately — bookended, so it cannot
> be lost to a re-cut.

---

## Before you start

- [ ] Open the page and let it finish loading. The button reads **Run** and is
      enabled when the engine is ready. If you start talking before it loads,
      your first click does nothing and you lose the room.
- [ ] Zoom the browser to **125–150%**. Console text is small on a projector.
- [ ] Have a terminal open beside it, already in the repo, for the manifest shot.
- [ ] Close every other tab. A notification popping up mid-take costs a re-shoot.
- [ ] If recording: 1080p minimum, and check the console text is legible in the
      recording rather than only on your screen.

---

## 0:00 — 0:25 · The hook

> "Almost every application on your machine keeps its data in SQLite. Your
> browser, your phone backup, your notes app. You've got dozens of `.db` files
> and no way to look inside any of them without installing something."

**Do:** land on the top of the page. Do not scroll yet.

> "This is zql. It opens them. And the whole thing is written against the Rust
> standard library — no crates at all."

---

## 0:25 — 0:50 · The manifest, first showing

**Do:** switch to the terminal. Run it live, do not show a screenshot.

```
$ cargo tree
zql v0.1.0
```

> "One node. That's the whole dependency tree. No crates, no vendored source,
> no FFI, no `unsafe`. Fourteen thousand lines of Rust and nothing underneath
> it but `std`."

**Do:** leave it on screen for a beat. Then switch back to the browser.

---

## 0:50 — 1:40 · The cold open

**Do:** click the preset **"The cold open — browser history"**. It runs on click.

> "That's a real SQLite file — a Firefox history database — being read in your
> browser right now. Not a recording, not a mock. zql's engine is compiled to
> WebAssembly, and it's walking that file's b-tree to answer this."

**Point at the column headers.**

> "See the types under each column name? `integer`, `oid 20`. That's the type
> zql advertises on the PostgreSQL wire — the same bytes a real client would
> receive."

**Do:** click **"Forget the table name"**.

> "And you don't have to remember what's in the file. Ask for the database
> without a table and it tells you what's inside. That's the thing you actually
> want when you're staring at `places.sqlite` at eleven at night."

---

## 1:40 — 2:30 · It's a real engine

**Do:** click **"GROUP BY over a CASE"**.

> "This isn't a toy that pattern-matches `SELECT *`. `GROUP BY` over a `CASE`
> expression, with `HAVING` and `ORDER BY`. The parser, the binder, the planner
> and the executor are all here."

**Do:** expand **"the plan the binder built"** under the result.

> "That's the actual operator tree. Scan, aggregate, sort, project — you can see
> what it decided to do."

**Do:** click **"Join two tables"**.

> "And it joins. That's the feature the whole tool exists for — one query
> language over unlike things. A SQLite table against a CSV against your
> filesystem."

**Do:** click **"Query the filesystem"**, then **"Read a CSV"**.

> "Same engine, different sources. Five of them."

---

## 2:30 — 3:15 · The details that are easy to get wrong

**Do:** click **"Three-valued logic"**.

> "SQL `NULL` isn't 'empty' — it's 'unknown'. `NULL = NULL` is not true, it's
> null. `FALSE AND NULL` is false, but `TRUE AND NULL` is null. Getting this
> wrong doesn't crash your engine, it just quietly returns the wrong rows,
> which is worse."

**Do:** click **"NULL is not ''"**.

> "And a null title is a different thing from an empty one. On the wire that's
> a length of minus one versus a length of zero — zql keeps them distinct all
> the way through."

**Do:** click **"A typo"**.

> "Errors are a feature. That's a real SQLSTATE, a caret pointing at the exact
> character, and a suggestion — because `form` is one transposition from `FROM`,
> and that's the typo everyone actually makes."

**Do:** click **"Try to write"**.

> "And it's read-only by construction. It doesn't say 'syntax error near
> INSERT'. It names the feature and says why it isn't there. There are zero
> file-write calls in the whole source."

---

## 3:15 — 3:50 · The bug worth admitting

**Do:** click into the console, type `SELECT ` then paste a long `1+1+1+...`
chain (or just say the next line while clicking the depth-limit example).

> "This one nearly shipped. About seventeen hundred `1+1+1`s — under four
> kilobytes — used to overflow the stack and kill the entire server. Not the
> connection: the *process*, taking every other client with it."

> "A stack overflow isn't a panic. It aborts instead of unwinding, so the
> `catch_unwind` around each connection couldn't catch it. I found it by fuzzing
> the running server, and now it's a bounded depth with its own error code."

*(This beat is optional if you're tight on time — but it lands well with
engineers, because everyone has shipped something like it.)*

---

## 3:50 — 4:30 · Nothing to install includes the client

**Do:** switch to the terminal.

```
$ zql ~/projects
zql 0.1.0 — listening on 127.0.0.1:5432
  connect with:  psql -h 127.0.0.1 -p 5432

$ psql -h 127.0.0.1
psql (16.2)
you=> SELECT ext, COUNT(*) AS n FROM files GROUP BY ext ORDER BY n DESC LIMIT 5;
```

> "It speaks the PostgreSQL wire protocol. So there's no client to install
> either — `psql` already works. That's thirty years of somebody else's C
> completing a handshake with a binary that has no dependencies."

> "Same for node-postgres, which matters more than it sounds: it parses results
> *by type*, so if I'd got a type OID wrong it would hand back a wrong
> JavaScript value rather than a plausible string. Both of them agree."

---

## 4:30 — 5:00 · The manifest, second showing, and close

**Do:** back to the terminal, run it again. This is the required shot.

```
$ cat Cargo.toml
[dependencies]

$ cargo tree
zql v0.1.0
```

> "Empty. Two hundred and ninety-five tests, three platforms green in CI, and a
> reproducible build — two clean builds, byte-identical."

> "Seventeen packages replaced by hand: the SQLite reader, the Postgres wire
> protocol, the SQL parser, the async runtime, the CSV parser, the date library,
> the HTTP server. It's all in `STDLIB.md`, including what each one cost."

**Do:** end on the terminal with `cargo tree` visible. Do not cut away early.

---

## If something breaks

- **The engine doesn't load.** Say so and move to the terminal demo — the binary
  is the product, the page is a convenience. Do not debug on camera.
- **A query is slow.** It won't be; everything here is single-digit
  milliseconds. If the page stalls, reload and use a preset.
- **Someone asks about HEIC / a feature it lacks.** Good — go to "What it does
  not do". A stated limit is a stronger answer than a hedge.

## Questions you should expect

**"Why no dependencies? Isn't that just harder?"**
> "It's the hackathon's rule, and it's a real exercise: you find out how much of
> what you reach for you actually understand. The SQLite reader taught me the
> format has a trap where the answer to 'how much payload fits on this page' is
> deliberately *not* 'as much as fits'."

**"How do you know the SQLite reader is right?"**
> "It's checked against Python's `sqlite3`, which wrote the fixtures. That
> direction matters — a reader checked against its own output only proves it's
> self-consistent, and that's exactly what a misunderstanding also produces. It
> caught two real bugs that way."

**"Is it fast?"**
> "Fast enough, and I don't claim more. Every query is a full scan and the joins
> are nested-loop. It's for inspecting files interactively, not for serving
> traffic — and the README says so."

**"Could I use this?"**
> "For reading, today. It's read-only, loopback by default, no auth and no TLS.
> Point it at a directory and open `psql`."
