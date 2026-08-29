//! `tail('file.log')` — a log file, streamed as it grows.
//!
//! The query never finishes. It reads the file from the start, then follows it,
//! emitting each new line as it is written. `SELECT line FROM tail('app.log')
//! WHERE line LIKE '%ERROR%'` is a live filter over a log.
//!
//! # This is why cancellation reaches into a source
//!
//! Every other operator checks the cancel flag *between* rows, which is enough
//! because rows keep arriving. Here they do not: between two log lines this
//! source is asleep, and a check that only runs when a row is produced would
//! never run at all. So the flag is checked inside the polling loop, and Ctrl-C
//! in `psql` stops the query within one poll interval instead of never.
//!
//! Without that, the best thirty seconds of the demo ends on a frozen terminal.
//!
//! # Following, honestly
//!
//! Polling with a short sleep, not a filesystem watch: the standard library has
//! no watch API, and the platform ones are three different APIs with three sets
//! of edge cases. At a 100 ms interval the latency is invisible to a person
//! reading a terminal and the cost is nothing.
//!
//! A partial final line — bytes written without a terminating newline — is held
//! back until the newline arrives. Emitting it early would split one log line
//! across two rows, which is worse than a moment's delay.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::error::{Result, SqlState, ZqlError};
use crate::exec::RowIter;
use crate::plan::schema::{Column, Schema};
use crate::server::cancel::CancelFlag;
use crate::sources::TableSource;
use crate::value::{Row, Type, Value};

/// How long to wait before looking for more data.
const POLL: Duration = Duration::from_millis(100);

pub struct TailSource {
    path: PathBuf,
    schema: Schema,
}

impl TailSource {
    pub fn open(path: &Path) -> Result<TailSource> {
        // Opened once here purely to fail at bind time if it cannot be read.
        File::open(path).map_err(|err| {
            ZqlError::new(
                SqlState::IoError,
                format!("cannot read {}: {err}", path.display()),
            )
        })?;

        Ok(TailSource {
            path: path.to_path_buf(),
            schema: Schema::new(vec![Column::new("line", Type::Text)]),
        })
    }
}

impl TableSource for TailSource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn scan(&self, cancel: &CancelFlag) -> Result<Box<dyn RowIter>> {
        let file = File::open(&self.path).map_err(|err| {
            ZqlError::new(
                SqlState::IoError,
                format!("cannot read {}: {err}", self.path.display()),
            )
        })?;

        Ok(Box::new(TailIter {
            reader: BufReader::new(file),
            path: self.path.clone(),
            position: 0,
            cancel: cancel.clone(),
        }))
    }
}

struct TailIter {
    reader: BufReader<File>,
    path: PathBuf,
    /// Bytes consumed so far, used to detect truncation.
    position: u64,
    cancel: CancelFlag,
}

impl RowIter for TailIter {
    /// Never returns `Ok(None)`.
    ///
    /// The only ways out are a cancellation, a client disconnect — which the
    /// session notices when its write fails — or the process ending.
    fn next(&mut self) -> Result<Option<Row>> {
        let mut line = String::new();

        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Err(ZqlError::canceled());
            }

            line.clear();
            let read = self.reader.read_line(&mut line).map_err(|err| {
                ZqlError::new(
                    SqlState::IoError,
                    format!("cannot read {}: {err}", self.path.display()),
                )
            })?;

            if read > 0 {
                // A line without a terminator is the tail of a write still in
                // progress. Rewind and wait for the rest of it.
                if !line.ends_with('\n') {
                    self.reader
                        .seek(SeekFrom::Start(self.position))
                        .map_err(|err| seek_error(&self.path, &err))?;
                    self.wait()?;
                    continue;
                }

                self.position += read as u64;
                let text = line.trim_end_matches('\n').trim_end_matches('\r');
                return Ok(Some(Row::new(vec![Value::Text(text.to_string())])));
            }

            // At the end of the file. Before sleeping, check whether the path
            // still refers to the file we are holding open.
            self.follow_rotation()?;
            self.wait()?;
        }
    }
}
impl TailIter {
    /// Sleeps for one poll interval, in short slices so that a cancellation is
    /// noticed promptly rather than after the whole interval.
    fn wait(&self) -> Result<()> {
        const SLICE: Duration = Duration::from_millis(20);
        let mut waited = Duration::ZERO;
        while waited < POLL {
            if self.cancel.load(Ordering::SeqCst) {
                return Err(ZqlError::canceled());
            }
            std::thread::sleep(SLICE);
            waited += SLICE;
        }
        Ok(())
    }

    /// Reopens the path when the file behind it has been rotated away.
    ///
    /// # The two shapes of rotation, and why length alone is not enough
    ///
    /// **Truncation in place** leaves the same file shorter than what we have
    /// already read, so `handle_len < position` catches it.
    ///
    /// **Rename-and-recreate** — what `logrotate` actually does — does not.
    /// Our handle still refers to the *old* file, which nothing will ever
    /// append to again, while the path now names a new one. The old file is
    /// permanently at EOF, so a follower that only watches for shrinkage waits
    /// forever. That is a hang, not a slowdown, and it was found by a test that
    /// deleted the file rather than truncating it.
    ///
    /// The discriminator is comparing the *handle's* length against the
    /// *path's*. Both grow together for an append to the same file; they differ
    /// only when the path has come to mean something else.
    fn follow_rotation(&mut self) -> Result<()> {
        let handle_len = self.handle_len();

        // Truncated in place: we have read past the end of our own file.
        if handle_len < self.position {
            return self.reopen();
        }

        let Ok(path_meta) = std::fs::metadata(&self.path) else {
            // Gone for the moment — the gap between the rename and the new
            // file appearing. Waiting is better than failing a query that is
            // about to recover on its own.
            return Ok(());
        };

        if path_meta.len() == handle_len {
            return Ok(()); // same file, nothing to do
        }

        // The lengths disagree. Either the path is a different file now, or a
        // writer appended between the two calls above — in which case our own
        // handle has grown too, and re-reading it settles which.
        if self.handle_len() != handle_len {
            return Ok(()); // an append; the existing handle will see it
        }

        self.reopen()
    }

    /// The length of the file this reader actually holds open, which is not
    /// necessarily the file the path names any more.
    fn handle_len(&self) -> u64 {
        self.reader
            .get_ref()
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(self.position)
    }

    fn reopen(&mut self) -> Result<()> {
        let file = File::open(&self.path).map_err(|err| {
            ZqlError::new(
                SqlState::IoError,
                format!("cannot reopen {}: {err}", self.path.display()),
            )
        })?;
        self.reader = BufReader::new(file);
        self.position = 0;
        Ok(())
    }
}

fn seek_error(path: &Path, err: &std::io::Error) -> ZqlError {
    ZqlError::new(
        SqlState::IoError,
        format!("cannot seek in {}: {err}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    #[test]
    fn a_missing_file_fails_at_bind_time() {
        let error = match TailSource::open(Path::new("no-such-log-file.log")) {
            Err(error) => error,
            Ok(_) => panic!("a missing file must not open"),
        };
        assert_eq!(error.state, SqlState::IoError);
    }

    #[test]
    fn existing_lines_are_read_before_following() {
        let path = scratch("zql-tail-existing.log");
        std::fs::write(&path, "first\nsecond\n").unwrap();

        let source = TailSource::open(&path).unwrap();
        let flag: CancelFlag = Arc::new(AtomicBool::new(false));
        let mut scan = source.scan(&flag).unwrap();

        assert!(
            matches!(scan.next().unwrap(), Some(row) if matches!(&row.0[0], Value::Text(t) if t == "first"))
        );
        assert!(
            matches!(scan.next().unwrap(), Some(row) if matches!(&row.0[0], Value::Text(t) if t == "second"))
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_partial_line_is_held_back_until_its_newline_arrives() {
        let path = scratch("zql-tail-partial.log");
        std::fs::write(&path, "complete\nincomp").unwrap();

        let source = TailSource::open(&path).unwrap();
        let flag: CancelFlag = Arc::new(AtomicBool::new(false));
        let mut scan = source.scan(&flag).unwrap();

        assert!(
            matches!(scan.next().unwrap(), Some(row) if matches!(&row.0[0], Value::Text(t) if t == "complete"))
        );

        // The partial line must not appear. Cancel to escape the follow loop.
        let waiter = Arc::clone(&flag);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            waiter.store(true, Ordering::SeqCst);
        });
        let error = scan.next().unwrap_err();
        assert_eq!(error.state, SqlState::QueryCanceled);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_line_appended_while_following_is_emitted() {
        let path = scratch("zql-tail-append.log");
        std::fs::write(&path, "one\n").unwrap();

        let source = TailSource::open(&path).unwrap();
        let flag: CancelFlag = Arc::new(AtomicBool::new(false));
        let mut scan = source.scan(&flag).unwrap();
        assert!(scan.next().unwrap().is_some());

        let appended = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&appended)
                .unwrap();
            writeln!(file, "two").unwrap();
        });

        let row = scan.next().unwrap().expect("the appended line");
        assert!(matches!(&row.0[0], Value::Text(text) if text == "two"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_rotated_log_is_picked_up_from_the_start_of_the_new_file() {
        // Log rotation, reproduced exactly: the file shrinks, then new content
        // arrives. A follower that keeps its old byte offset would either sit
        // past the end of the new file forever or resume mid-line.
        let path = scratch("zql-tail-rotate.log");
        std::fs::write(&path, "before-one\nbefore-two\n").unwrap();

        let source = TailSource::open(&path).unwrap();
        let flag: CancelFlag = Arc::new(AtomicBool::new(false));
        let mut scan = source.scan(&flag).unwrap();

        // Consume the pre-rotation content so the reader is at the end.
        for expected in ["before-one", "before-two"] {
            let row = scan.next().unwrap().expect("a pre-rotation line");
            assert!(matches!(&row.0[0], Value::Text(t) if t == expected));
        }

        // Rotate: replace the file with a shorter one, as logrotate does.
        let rotating = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            std::fs::write(&rotating, "after-one\n").unwrap();
        });

        let row = scan.next().unwrap().expect("the post-rotation line");
        assert!(
            matches!(&row.0[0], Value::Text(t) if t == "after-one"),
            "expected the rotated file's first line, got {:?}",
            row.0[0]
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_log_that_vanishes_mid_follow_is_waited_for_rather_than_failing() {
        // The window during a rotation when the old file is gone and the new
        // one has not been created. Failing the query here would make `tail()`
        // unusable against any log that rotates.
        let path = scratch("zql-tail-vanish.log");
        std::fs::write(&path, "first\n").unwrap();

        let source = TailSource::open(&path).unwrap();
        let flag: CancelFlag = Arc::new(AtomicBool::new(false));
        let mut scan = source.scan(&flag).unwrap();
        assert!(scan.next().unwrap().is_some());

        std::fs::remove_file(&path).unwrap();

        // Recreate it after a beat, as a rotation would.
        let recreated = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            std::fs::write(&recreated, "recreated\n").unwrap();
        });

        let row = scan.next().unwrap().expect("the recreated file's line");
        assert!(matches!(&row.0[0], Value::Text(t) if t == "recreated"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_set_cancel_flag_stops_the_follow_loop_rather_than_hanging() {
        // The failure this whole mechanism exists to prevent.
        let path = scratch("zql-tail-cancel.log");
        std::fs::write(&path, "only\n").unwrap();

        let source = TailSource::open(&path).unwrap();
        let flag: CancelFlag = Arc::new(AtomicBool::new(false));
        let mut scan = source.scan(&flag).unwrap();
        assert!(scan.next().unwrap().is_some());

        flag.store(true, Ordering::SeqCst);
        let error = scan.next().unwrap_err();
        assert_eq!(error.state, SqlState::QueryCanceled);

        let _ = std::fs::remove_file(&path);
    }
}
