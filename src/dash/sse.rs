//! Server-sent events, and the set of browsers listening to them.
//!
//! Stands in for a websocket library. SSE is the right shape here: the traffic
//! is one-directional, it is plain HTTP so nothing has to be upgraded, and the
//! browser reconnects on its own.
//!
//! Three details in here were found by spiking rather than by designing, and
//! each one is a way this deadlocks or leaks without them.

use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// How long a write to one browser may take before that browser is abandoned.
///
/// **Without this the dashboard deadlocks.** A client that stops reading but
/// does not disconnect — a laptop lid closing, a tab throttled in the
/// background — fills its socket buffer, and an unbounded write to it blocks
/// the producer thread forever, freezing the dashboard for everyone else.
const WRITE_TIMEOUT: Duration = Duration::from_millis(500);

/// Tells the browser how long to wait before reconnecting.
///
/// Sent as the first line of every stream, so the dashboard heals itself if the
/// server is restarted mid-demo instead of needing a manual refresh.
const RETRY_HINT: &str = "retry: 2000\n\n";

/// The set of connected dashboards.
#[derive(Default)]
pub struct Broadcaster {
    clients: Mutex<Vec<TcpStream>>,
    delivered: AtomicU64,
}

impl Broadcaster {
    pub fn new() -> Self {
        Broadcaster::default()
    }

    /// Adopts a socket whose SSE headers have already been written.
    ///
    /// The request thread hands the socket over and **ends**. Parking a thread
    /// per live client instead would mean fifty dashboards cost fifty blocked
    /// threads, all of them doing nothing but holding a stack.
    pub fn subscribe(&self, mut stream: TcpStream) {
        let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
        if stream.write_all(RETRY_HINT.as_bytes()).is_err() {
            return;
        }
        let _ = stream.flush();
        self.lock().push(stream);
    }

    /// Sends one `data:` event to every listener.
    pub fn publish(&self, payload: &str) {
        // A payload containing a newline would be read as two events, or as
        // the end of one. Every line gets its own `data:` prefix, which is what
        // the SSE format specifies for multi-line data.
        let mut frame = String::with_capacity(payload.len() + 16);
        for line in payload.split('\n') {
            frame.push_str("data: ");
            frame.push_str(line);
            frame.push('\n');
        }
        frame.push('\n');

        self.write_to_all(&frame);
        self.delivered.fetch_add(1, Ordering::Relaxed);
    }

    /// A comment frame, sent on a timer.
    ///
    /// **Required, and this was the spike's one real finding.** Dead clients
    /// are pruned *by the broadcast itself*, and zql only broadcasts when a
    /// query runs — so with nobody querying, a closed browser tab holds its
    /// socket indefinitely. The heartbeat does three jobs for five lines:
    /// prunes regardless of query activity, stops intermediaries dropping an
    /// idle connection, and shows liveness on the demo video when nothing else
    /// is happening.
    pub fn heartbeat(&self) {
        self.write_to_all(": ping\n\n");
    }

    /// Writes to every client, dropping the ones that fail.
    ///
    /// `retain_mut` **is** the entire pruning mechanism: a failed write means
    /// the browser is gone. No bookkeeping, no reaper thread, no liveness
    /// protocol — the thing that proves a client is alive is successfully
    /// sending to it.
    fn write_to_all(&self, frame: &str) {
        let bytes = frame.as_bytes();
        self.lock().retain_mut(|client| {
            client.write_all(bytes).is_ok() && client.flush().is_ok()
        });
    }

    pub fn client_count(&self) -> usize {
        self.lock().len()
    }

    /// A poisoned lock means another thread panicked while holding it. The
    /// list is plain data that a panic cannot corrupt, so recovering beats
    /// disabling the dashboard for the rest of the process.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<TcpStream>> {
        self.clients
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Escapes a string for embedding in JSON.
///
/// Stands in for `serde_json`, for the one direction zql needs. A query can
/// contain quotes, backslashes and newlines, and any of them unescaped
/// produces a frame the browser silently discards — SSE failures are quiet,
/// which is what makes them expensive to debug.
pub fn escape_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Control characters must be escaped as \u00XX or the JSON is
            // invalid; everything else, including all of Unicode, passes
            // through as UTF-8.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escaping_covers_what_a_query_can_contain() {
        assert_eq!(escape_json(r#"a "b" c"#), r#"a \"b\" c"#);
        assert_eq!(escape_json(r"C:\logs"), r"C:\\logs");
        assert_eq!(escape_json("two\nlines"), "two\\nlines");
        assert_eq!(escape_json("tab\there"), "tab\\there");
        assert_eq!(escape_json("\u{1}"), "\\u0001");
        // Unicode is valid JSON as-is; escaping it would only bloat the frame.
        assert_eq!(escape_json("写真 🎞"), "写真 🎞");
    }

    #[test]
    fn a_broadcaster_with_no_clients_is_harmless() {
        let broadcaster = Broadcaster::new();
        broadcaster.publish("{}");
        broadcaster.heartbeat();
        assert_eq!(broadcaster.client_count(), 0);
    }
}
