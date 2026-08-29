# Spike — the live dashboard (HTTP/1.1 + Server-Sent Events)

> Run 2026-08-17 in `scratchpad/ssespike/`. ~180 lines Rust, empty `[dependencies]`,
> compiled clean. **All nine tests passed**, verified against **Node 22's HTTP client** and
> **a real headless Chrome `EventSource`**.
>
> **Throwaway — delete before kickoff.** This document is what survives it.

---

## 1. Why this needed a spike

The dashboard was the only major zql module with **zero evidence** behind it. 400 lines
budgeted, and the prior-art corpus is unambiguous that static projects lose — so it is
protected on the cut list. Protecting something unproven is how hour 38 goes wrong.

---

## 2. Results

### Against Node 22's HTTP client — an independent HTTP/1.1 implementation

| # | Test | Result |
|---|---|---|
| 1 | Two clients receive a live broadcast | ✅ 8 events each in 2 s |
| 2 | `Content-Type: text/event-stream` | ✅ |
| 3 | No `Content-Length` on the stream | ✅ — the connection stays open |
| 4 | Page served while streams are open | ✅ 200, 780 bytes, concurrent |
| 5 | Abruptly destroyed client stops | ✅ frozen at 8 events |
| 6 | Surviving client keeps going | ✅ 8 → 18 events |
| 7 | 50 rapid connect/disconnect cycles | ✅ all pruned |
| 8 | Server healthy after churn | ✅ still streaming |
| 9 | Still serving pages after churn | ✅ 200 |

Tests 5–6 are the important pair: a client ripped away with **no FIN handshake** must not
block the producer thread and take every other client down with it.

### Against real Chrome (headless), a real `EventSource`

Node proves the HTTP framing. It does not prove a browser's `EventSource` — which is picky
about the exact content type and `\n\n` framing and fails *silently* when it is unhappy.

The page reports its own state back to the server, which makes the test decisive:

```
[srv] /events client #1 attached
[srv] *** BROWSER REPORT: /report?ev=open
[srv] *** BROWSER REPORT: /report?ev=got5&ua=Mozilla%2F5.0%20(Windows%20NT%2010.0%3B%20
[srv] pruned 1 dead client(s), 0 live
```

`ev=open` — the browser accepted the stream. `ev=got5` — it parsed and rendered five events.
Then the tab was killed and the server pruned it. **End to end in a real browser.**

*(Note for anyone repeating this: `chrome --dump-dom --virtual-time-budget` **hangs** on an
SSE page, because the load never reaches network-idle. Self-reporting from the page is the
way around it.)*

---

## 3. The design, confirmed

- **One thread per *request*, not per live client.** The `/events` handler writes the
  headers, pushes its socket into a shared `Vec<TcpStream>`, and **ends**. The producer
  owns the sockets from then on. 50 concurrent dashboards do not mean 50 parked threads.
- **`retain_mut` is the entire pruning mechanism.** Broadcast walks the vector; a failed
  write means the client is gone and it is dropped. No bookkeeping, no reaper thread.
- **Bound the write.** `set_write_timeout(500ms)` on each client socket. Without it, one
  client that has stopped reading — but not disconnected — blocks the producer forever and
  freezes the dashboard for everyone. This is the deadlock the spike was really testing for.
- **Send `retry: 2000` as the first line.** It tells the browser how fast to reconnect, so
  the dashboard heals itself if the server restarts mid-demo.
- **Parse only the request line and headers, never a body.** There are no POSTs. Reading a
  body you never expect is where hand-rolled HTTP servers hang.

---

## 4. The one real finding — a heartbeat is required

**Pruning only happens inside the broadcast.** In the spike the producer ticks every 250 ms,
so dead clients are reaped almost immediately. **In zql the producer only fires when a query
runs** — so if nobody queries for an hour, a closed browser tab holds its socket for an hour.

Not fatal, but it is a leak, and it is exactly the sort of thing a judge reading the code
would notice.

**Fix:** a comment heartbeat — `: ping\n\n` — every ~15 s from a timer thread. It costs about
five lines and does three jobs at once:

1. prunes dead clients regardless of query activity,
2. keeps intermediaries from closing an idle connection,
3. proves liveness on the demo video when nothing is happening.

**This was not in the design. It is now** — `ARCHITECTURE.md` §4.8.

---

## 5. Verdict

**The dashboard is proven and stays protected on the cut list.** The risky parts — holding
connections open by hand, broadcasting from a producer thread, surviving abrupt
disconnects, and satisfying a real browser's `EventSource` — all work with an empty
manifest.

Revised estimate: **~405 lines** (400 + the heartbeat), unchanged in practice.
