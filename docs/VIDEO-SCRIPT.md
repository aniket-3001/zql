# zql — the five-minute submission video

> **One take, 4:45 of content in a 5:00 budget.** Demo first as a user, then
> the engineering. Nothing is explained before it has been shown.
>
> Everything happens at **https://aniket-3001.github.io/zql/** plus one
> terminal. Word counts are set for ~150 wpm — a comfortable pace, not a rush.
>
> **The one hard requirement:** the rules say the video must show the tool
> **and the empty manifest**. Forgetting the manifest is the commonest way to
> lose points, so it appears at **0:15** and again at **4:30**. If you cut
> anything, do not cut those.

---

## Before you record

- [ ] Load the page. Wait for **Run** to enable — if you start talking before
      the engine loads, your first click does nothing.
- [ ] Browser zoom **150%**. Text that is comfortable on your monitor is
      unreadable in a 1080p recording.
- [ ] Terminal open in the repo, font size up, window already sized. You switch
      to it twice and should never be seen resizing anything.
- [ ] Close other tabs and silence notifications.
- [ ] Click through all five presets once as a rehearsal, then reload.

**The demo half is 5 clicks.** Nothing is typed except one short edit at 2:10.

---

# PART ONE · THE DEMO (0:00 – 2:40)

## 0:00 – 0:15 · The hook

**Screen:** top of the page.

> "Your browser history is a SQLite file sitting on your disk right now. So is
> your phone backup, your notes app, your photo library.
>
> You have dozens of these — and you can't open any of them without installing
> something."

## 0:15 – 0:30 · The manifest, first showing

**Do:** switch to the terminal. Run it live.

```
$ cargo tree
zql v0.1.0
```

> "This is zql. It opens them. And that's its entire dependency tree — one node.
> No crates, no vendored code, no FFI. Fourteen thousand lines of Rust on
> nothing but the standard library."

**Do:** switch back to the browser. *(Leave the terminal on `cargo tree` — you
come back to this exact screen at 4:30.)*

## 0:30 – 1:05 · Open the thing you couldn't open

**Do:** click preset **1 · Open my browser history**.

> "So — here's a real Firefox history database, and here's the last ten sites I
> spent time on. That took four milliseconds."

**Do:** point at the column headers.

> "And this is running *in the browser*. The engine is compiled to WebAssembly,
> and it's walking that file's b-tree right now. Nothing is being sent to a
> server — there is no server."

**Do:** click preset **2 · What's even in this file?**

> "You don't have to know what's inside first. Ask for the file without a table
> name and it tells you: `moz_places`, `moz_bookmarks`. That's the thing you
> actually want at eleven at night when you're staring at an unfamiliar `.db`."

## 1:05 – 2:10 · The question no other tool answers

**Do:** click preset **3 · Did I read what people sent me?**

> "Now the reason I built this."

*(Let the result land for a beat before talking. It's seven rows — readable.)*

> "I keep a reading list — links people send me — in a CSV. My browsing history
> is in SQLite. Two files, two completely different formats, and the question I
> want to ask spans both of them: *which of these did I actually read?*"

**Do:** point at the bottom two rows, which read `0`.

> "Turns out I never opened the paper Alex sent me. Or Priya's DNS post. Sorry,
> both of you."

> "That's a LEFT JOIN across a CSV and a SQLite database — and those zeros are
> real: the join finds no match, and `COUNT` skips nulls, so they come back zero
> instead of one. It's the kind of thing that's easy to get subtly wrong."

> "There's no ordinary way to ask this. You'd export one, import the other,
> write a script. Here it's one query, and it's the same query language for
> both — plus your filesystem, log files, and environment variables. Five
> sources, and they all join to each other."

**Do:** click into the console, change `DESC` to `ASC` on the last line, and
press **Ctrl+Enter**. The two zeros jump to the top.

> "And it's live — I can just edit it. Worst offenders first."

## 2:10 – 2:40 · It can't hurt you, and it helps when you slip

**Do:** click preset **4 · It can't touch my data**.

> "One thing that matters when you're pointing a tool at your own files: it is
> read-only by construction. Not 'syntax error near INSERT' — it names the
> feature and tells you why it isn't there. There are zero file-write calls in
> the entire source."

**Do:** click preset **5 · When I typo**.

> "And when you typo — which is constantly — you get a real Postgres error code,
> a caret under the exact character, and a suggestion. `form` is one
> transposition from `FROM`, and that's the mistake everybody actually makes."

---

# PART TWO · HOW IT'S BUILT (2:40 – 4:45)

**Do:** scroll to **How it works**. *(Or press Present and use → if you prefer
full-screen slides.)*

## 2:40 – 3:05 · The shape of it

> "So what is it actually? A read-only SQL engine that speaks the PostgreSQL
> wire protocol."

> "That last part is the decision I'd defend hardest. 'Nothing to install' has
> to include the *client* — otherwise I've just moved the problem. Because it
> speaks Postgres, `psql` already works. Every Postgres client already works."

**Do:** point at the pipeline diagram.

> "Wire protocol, lexer, parser, binder, executor, sources. The interesting
> constraint is that the protocol makes you send the *shape* of a result before
> its contents — so every column name and type has to be resolved before the
> first row is read. That's why binding is its own phase, and it shapes the
> whole program."

## 3:05 – 3:35 · Seventeen packages, by hand

**Do:** scroll to **Seventeen packages, written by hand**.

> "With an empty manifest, everything you'd normally install, you write.
> Seventeen substitutions here — the SQLite file format, the Postgres wire
> protocol, the SQL parser, the async runtime, the CSV parser, the date
> library."

> "The SQLite reader was the one with real teeth. There's a rule in that format
> for how much of a row lives on its own page, and the answer is deliberately
> *not* 'as much as fits' — it's a smaller number, so pages stay dense. Get it
> wrong and small rows read perfectly while large ones come back quietly
> corrupted. That's the kind of bug that survives casual testing."

## 3:35 – 4:10 · How I know it's right

**Do:** scroll to **How I know it's right**.

> "Which brings up the thing I'd most want a reviewer to look at: how do you
> trust any of this?"

> "Everything is checked against *other people's* implementations, never against
> itself. A reader checked against its own output only proves it's
> self-consistent — and self-consistency is exactly what a misunderstanding
> also produces."

> "So the test fixtures are written by Python's `sqlite3`, and the expected
> values come from `sqlite3`, not from me. That caught two real bugs. An
> `INTEGER PRIMARY KEY` reads as null in every row, because the value lives in
> the cell header rather than the record. And a `REAL` column holding
> three-seventy-five-point-zero is stored as an *integer*. Both looked
> completely plausible in the output. Only the oracle caught them."

> "Same on the protocol side — it's checked against real `psql` and against
> node-postgres, which parses results by type, so a wrong type code gives it a
> wrong JavaScript value rather than a plausible-looking string."

## 4:10 – 4:30 · The one I nearly shipped

> "And one I want to be honest about. A four-kilobyte query — about seventeen
> hundred `1+1+1`s — used to kill the entire server. Not the connection: the
> process, taking every other connected client with it."

> "A stack overflow isn't a panic. It aborts instead of unwinding, so the
> `catch_unwind` I had around every connection couldn't catch it. I found it by
> fuzzing the running server, and it's now a bounded depth — measured, not
> guessed, because my first guess was wrong by a factor of ten."

## 4:30 – 4:45 · The manifest, second showing, and close

**Do:** switch to the terminal — the same screen you left at 0:30.

```
$ cargo tree
zql v0.1.0
```

> "One node. Two hundred and ninety-five tests, green on Linux, macOS and
> Windows. A reproducible build — two clean builds, byte-identical."

> "Everything you saw is live at that URL, and the source is on GitHub. Thanks."

**Do:** hold on `cargo tree` for two full seconds before you stop recording.

---

## Timing

| | Beat | Length | Ends |
|---|---|---|---|
| **Demo** | The hook | 0:15 | 0:15 |
| | **Manifest ①** | 0:15 | 0:30 |
| | Open the file | 0:35 | 1:05 |
| | **The CSV × SQLite question** | 1:05 | 2:10 |
| | Read-only, and errors | 0:30 | 2:40 |
| **Build** | The shape of it | 0:25 | 3:05 |
| | Seventeen by hand | 0:30 | 3:35 |
| | How I know it's right | 0:35 | 4:10 |
| | The bug | 0:20 | 4:30 |
| | **Manifest ② + close** | 0:15 | 4:45 |

**15 seconds of slack.** Spend it on the join result at 1:05, not on the
engineering half.

**On pacing:** the spoken words here total **509**, which is about **3 minutes
25 seconds** of actual talking at a natural pace. The remaining ~80 seconds is
deliberate — clicks, two window switches, and letting each result sit on screen
before you speak over it. If you find yourself with nothing to say while a
result loads, that is the script working. Do not fill it.

### If you run long

Cut in this order — each is self-contained:

1. **The `ASC` edit at 2:05** (−10s)
2. **The bug, 4:10–4:30** (−20s) — it's the best story but the least necessary
3. **Preset 2, "What's even in this file?"** (−15s)
4. **Seventeen packages, 3:05–3:35** → cut to one sentence (−20s)

Never cut: the join at 1:05, or either manifest showing.

### If you run short

Add the plan tree — after preset 3, expand **"the plan the binder built"**:

> "And it'll show you what it decided to do — scan, join, aggregate, sort."

---

## If something goes wrong

- **The engine doesn't load.** Say "let me show you the real thing instead" and
  do it in the terminal with `psql`. The binary is the product; the page is a
  convenience. Do not debug on camera.
- **You misclick a preset.** Don't apologise or restart — click the right one
  and carry on. A one-take video with a recovered misclick reads as live, which
  is exactly what you want.
- **A result looks different from rehearsal.** It won't; the data is fixed and
  the demo query is a deploy gate. If it does, read what's on screen rather than
  what you rehearsed.

## The line to remember

If you forget everything else, this is the video in one sentence:

> **"Two files, two formats, one question — and the whole thing has an empty
> dependency list."**
