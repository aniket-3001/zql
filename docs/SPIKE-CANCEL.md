# Spike — query cancellation

> Run 2026-08-17 in `scratchpad/cancelspike/`. ~260 lines Rust, empty
> `[dependencies]`, compiled clean with no warnings. **All four tests passed, and the
> decisive one ran against real `psql` 16.2 with a real Ctrl-C.**
>
> **The spike is throwaway and must be deleted before kickoff.** This document is what
> survives it.

---

## 1. The claim under test

`ARCHITECTURE.md` §4.5 asserted that a Postgres client wanting to stop a running query does
**not** close its connection — it opens a **second** TCP connection, sends a
`CancelRequest` carrying the PID and secret key from `BackendKeyData`, then goes back to
waiting on the first connection.

If true, a server that ignores that second connection leaves `tail()` unstoppable and the
client wedged forever — with no error, at the best moment of the demo.

It was asserted from reading. Now it is verified.

---

## 2. Results

### Against a protocol client (my model of psql)

| # | Test | Result |
|---|---|---|
| 1 | Cancel an endless query | ✅ `ErrorResponse` **SQLSTATE 57014** after 5 rows |
| 2 | Is the connection usable afterwards? | ✅ next query returned 3 rows and a clean `CommandComplete` |
| 3 | Cancel with the **wrong** secret key | ✅ ignored — query ran on to 41 rows |
| 4 | Control: nobody cancels | ✅ hung forever — **the bug being prevented, reproduced** |

Test 4 matters as much as test 1. It demonstrates that without this mechanism the failure is
real, not theoretical.

### Against real `psql` 16.2 with a real Ctrl-C

Driven by attaching to psql's console and issuing a genuine `CTRL_C_EVENT` via
`GenerateConsoleCtrlEvent`, so this is psql's actual behaviour and not a re-implementation
of it.

```
[srv] SSLRequest -> N
[srv] session pid=4004 secret=1302818513
[srv] pid 4004 query: SELECT line FROM tail('server.log');
[srv] *** CancelRequest arrived: pid=4004 secret=1302818513
[srv]     key matched -> flag set for pid 4004
[srv] pid 4004 cancelled after 395 rows
[srv] pid 4004 terminate
```

**Real psql opened a second connection and sent back the exact PID and secret we
advertised.** The model was right.

Bonus confirmation in the same run: **real psql leads with `SSLRequest`**, so the bare-`N`
reply at gate G0 is not optional — it is the very first thing a real client does.

---

## 3. The mechanism, confirmed

Three pieces, ~60 lines total:

1. **A registry** — `Mutex<HashMap<i32, (i32, Arc<AtomicBool>)>>` mapping the fake PID we
   advertised to (the secret we advertised, a flag the query watches).
2. **The cancel path** — a connection whose startup code is `80877102` carries PID and
   secret, matches them against the registry, sets the flag, and **closes with no reply**.
   The protocol specifies no response, and psql does not wait for one.
3. **The check** — the executor tests the flag **between rows** and returns
   `57014 canceling statement due to user request`.

### Four details that only show up when you build it

- **Check between rows, never inside one.** A half-written `DataRow` desynchronises the
  stream and the client never recovers. The check belongs at the top of the operator loop.
- **Reset the flag when a new query starts**, not when the cancel is consumed. Otherwise a
  cancel that arrives just after a query ends poisons the next one.
- **The connection must survive.** After `ErrorResponse`, send `ReadyForQuery` and carry
  on — test 2 confirms the session is fully reusable. Closing the socket would be wrong and
  would look like a crash.
- **Remove the PID from the registry on disconnect**, or the map grows for the process
  lifetime.

### Security posture, stated plainly

The secret key comes from `SystemTime` nanos XORed with a constant, because std has no RNG.
Test 3 shows a wrong key is refused, so it is not *nothing* — but it is guessable, and the
README will say so rather than implying protection it does not have. Nothing behind it is
sensitive: the worst case is cancelling your own query.

---

## 4. Verdict

**Cancellation is proven and stays Core.** ~60 lines, confirmed against the real client,
and without it the single best thirty seconds of the demo video ends in a frozen terminal.

The earlier note in `OVERVIEW.md` — *"CancelRequest → close the connection"* — was wrong and
has been corrected.
