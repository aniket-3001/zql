//! The cancellation registry.
//!
//! A Postgres client that wants to stop a running query does **not** close its
//! connection. It opens a *second* TCP connection, sends a `CancelRequest`
//! carrying the PID and secret key the server handed out in `BackendKeyData`,
//! and then goes back to waiting on the first connection. Verified against real
//! `psql` 16.2 driven by a real Ctrl-C.
//!
//! A server that ignores that second connection leaves an endless query —
//! `tail()`, in zql's case — unstoppable, with no error and no way out.
//!
//! Four details that only surface once this is built:
//!
//! 1. **Check the flag between rows, never inside one.** A half-written
//!    `DataRow` desynchronises the stream and the client never recovers.
//! 2. **Reset the flag when a query starts**, not when a cancel is consumed,
//!    or a cancel arriving just after a query ends poisons the next one.
//! 3. **The session survives.** After the `ErrorResponse` comes a
//!    `ReadyForQuery` and the connection carries on; closing it would look like
//!    a crash.
//! 4. **Unregister on disconnect**, or the map grows for the process lifetime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The flag a running query watches. Cloned into the executor, set by whoever
/// handles the cancel connection.
pub type CancelFlag = Arc<AtomicBool>;

struct Entry {
    secret: i32,
    flag: CancelFlag,
}

/// Process-wide map from the PID zql advertised to the session's cancel flag.
#[derive(Default)]
pub struct Registry {
    sessions: Mutex<HashMap<i32, Entry>>,
}

impl Registry {
    pub fn new() -> Self {
        Registry::default()
    }

    /// Registers a session and returns the flag its queries should watch.
    pub fn register(&self, pid: i32, secret: i32) -> CancelFlag {
        let flag: CancelFlag = Arc::new(AtomicBool::new(false));
        self.lock()
            .insert(pid, Entry {
                secret,
                flag: Arc::clone(&flag),
            });
        flag
    }

    /// Handles a `CancelRequest`. Returns whether it matched a live session,
    /// which is logged but never reported to the client — the protocol
    /// specifies no reply, and telling a caller whether a guess was right would
    /// turn the secret into an oracle.
    pub fn cancel(&self, pid: i32, secret: i32) -> bool {
        match self.lock().get(&pid) {
            Some(entry) if entry.secret == secret => {
                entry.flag.store(true, Ordering::SeqCst);
                true
            }
            _ => false,
        }
    }

    pub fn unregister(&self, pid: i32) {
        self.lock().remove(&pid);
    }

    /// A poisoned lock means some other thread panicked while holding it. The
    /// map itself is a `HashMap` of plain data and cannot be left inconsistent
    /// by a panic, so recovering is strictly better than propagating: refusing
    /// to cancel queries for the rest of the process lifetime is a worse
    /// outcome than reading a map that is known to be intact.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i32, Entry>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_matching_key_sets_the_flag() {
        let registry = Registry::new();
        let flag = registry.register(4004, 1234);
        assert!(!flag.load(Ordering::SeqCst));

        assert!(registry.cancel(4004, 1234));
        assert!(flag.load(Ordering::SeqCst));
    }

    #[test]
    fn a_wrong_secret_is_ignored() {
        let registry = Registry::new();
        let flag = registry.register(4004, 1234);

        assert!(!registry.cancel(4004, 9999));
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn an_unknown_pid_is_ignored() {
        let registry = Registry::new();
        registry.register(4004, 1234);
        assert!(!registry.cancel(5005, 1234));
    }

    #[test]
    fn unregistering_stops_later_cancels_from_matching() {
        let registry = Registry::new();
        registry.register(4004, 1234);
        registry.unregister(4004);
        assert!(!registry.cancel(4004, 1234));
    }

    #[test]
    fn the_flag_is_shared_with_the_session_not_copied() {
        let registry = Registry::new();
        let flag = registry.register(1, 1);
        registry.cancel(1, 1);
        // The executor holds this same handle while the query runs.
        assert!(flag.load(Ordering::SeqCst));
    }
}
