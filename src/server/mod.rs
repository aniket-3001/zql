//! Connection lifecycle and session state.
//!
//! One `std::thread` per connection, blocking IO, no async runtime. A demo
//! server has single-digit clients; a thread pool here would be complexity
//! bought with nothing. Shared state is deliberately tiny — the cancel registry
//! is the whole of it at this gate — and each piece carries its own lock rather
//! than there being any global mutable state.

pub mod cancel;
pub mod startup;

use std::io::{self, BufReader, BufWriter, Write};
use std::net::{TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::dash::{self, sse::Broadcaster, QueryEvent};
use crate::error::{Result, ZqlError};
use crate::exec;
use crate::plan::schema::{Column, Schema};
use crate::plan::{self, plan::Plan};
use crate::sources::{self, cache::{CacheActivity, FileIndexCache}, SourceConfig};
use crate::sql;
use crate::sql::ast::Statement;
use crate::value::{Row, Type, Value};
use crate::wire::backend::{self, TransactionStatus};
use crate::wire::frontend::{self, Frontend};
use crate::wire::Message;

use cancel::{CancelFlag, Registry};
use startup::{Parameters, Startup};

/// The version zql reports to clients.
///
/// Claiming a real PostgreSQL version rather than `zql 0.1` is not vanity: libpq
/// and node-postgres both branch on it, and a client that cannot parse the
/// string disables features or refuses the connection outright.
const SERVER_VERSION: &str = "16.2";

/// How long a client has to complete its startup handshake.
///
/// Generous for a real client, which sends its packet immediately, and short
/// enough that a silent connection cannot hold a thread indefinitely.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Runtime configuration, fixed at startup.
#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    /// The directory the `files` source walks when no path is given.
    pub dir: PathBuf,
    /// Whether the filesystem index is cached for the life of a session.
    pub cache: bool,
    /// The dashboard's port, if it was asked for.
    pub dashboard: Option<u16>,
}

/// Shared server state.
pub struct Server {
    config: Config,
    cancels: Arc<Registry>,
    /// Source of the fake PIDs advertised in `BackendKeyData`.
    next_pid: AtomicI32,
    /// Connected dashboards. `None` unless `--dashboard` was passed.
    dashboard: Option<Arc<Broadcaster>>,
}

impl Server {
    pub fn new(config: Config) -> Self {
        Server {
            cancels: Arc::new(Registry::new()),
            next_pid: AtomicI32::new(1),
            dashboard: None,
            config,
        }
    }

    /// Binds the listening socket and serves until the process is stopped.
    pub fn run(self) -> io::Result<()> {
        let listener = TcpListener::bind((self.config.host.as_str(), self.config.port))?;
        self.serve_on(listener)
    }

    /// Serves on an already-bound listener.
    ///
    /// Split out from [`run`](Self::run) so a caller can bind port 0 and learn
    /// the assigned port before any client connects — which is what the
    /// connection-guard test needs, and what any embedding of the server would
    /// need too.
    pub fn serve_on(self, listener: TcpListener) -> io::Result<()> {
        let address = listener.local_addr()?;

        println!("zql {} — listening on {address}", env!("CARGO_PKG_VERSION"));
        println!("  connect with:  psql -h {} -p {}", self.config.host, address.port());

        let mut server = self;
        if let Some(port) = server.config.dashboard {
            server.dashboard = dash::start(&server.config.host, port, address.port());
        }
        let server = Arc::new(server);
        for incoming in listener.incoming() {
            match incoming {
                Ok(stream) => {
                    let server = Arc::clone(&server);
                    // A failed spawn is worth reporting but not worth dying
                    // over: the listener is still healthy and the next client
                    // may well succeed.
                    if let Err(err) = thread::Builder::new()
                        .name("zql-connection".to_string())
                        .spawn(move || server.handle(stream))
                    {
                        eprintln!("zql: cannot spawn connection thread: {err}");
                    }
                }
                Err(err) => eprintln!("zql: rejected connection: {err}"),
            }
        }
        Ok(())
    }

    /// Runs one connection to completion, catching a panic rather than letting
    /// it take the process down.
    ///
    /// Every parse and every read in zql is fallible by construction, so this
    /// should never fire. It exists because "should never" is not a guarantee,
    /// and one malformed database file must not be able to end the server for
    /// every other client.
    fn handle(&self, stream: TcpStream) {
        let peer = stream
            .peer_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());

        let outcome = catch_unwind(AssertUnwindSafe(|| self.serve(stream)));

        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(err)) if is_disconnect(&err) => {}
            Ok(Err(err)) => eprintln!("zql: connection from {peer} ended: {err}"),
            Err(_) => eprintln!("zql: connection from {peer} panicked; connection dropped"),
        }
    }

    fn serve(&self, stream: TcpStream) -> io::Result<()> {
        // Nagle's algorithm would hold back the small messages that make up a
        // handshake, adding latency to every connection for no benefit.
        stream.set_nodelay(true)?;

        // A handshake must be prompt. Without a bound here, a client that
        // connects and then says nothing parks a thread for the lifetime of the
        // process, and enough of them exhaust the server without sending a
        // single valid byte.
        //
        // The bound covers *only* the handshake and is cleared below: an
        // established session sitting idle at a `psql` prompt is entirely
        // legitimate and must never be timed out.
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;

        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = BufWriter::new(stream);

        let startup = startup::negotiate(&mut Duplex {
            reader: &mut reader,
            writer: &mut writer,
        })?;
        reader.get_ref().set_read_timeout(None)?;

        match startup {
            Startup::Cancel { pid, secret } => {
                // No reply, ever. The client is not listening on this socket;
                // it went back to waiting on its first connection.
                let matched = self.cancels.cancel(pid, secret);
                if matched {
                    println!("zql: cancel accepted for pid {pid}");
                }
                Ok(())
            }
            Startup::Connect(parameters) => self.session(reader, writer, parameters),
        }
    }

    fn session(
        &self,
        mut reader: BufReader<TcpStream>,
        mut writer: BufWriter<TcpStream>,
        parameters: Parameters,
    ) -> io::Result<()> {
        let (pid, secret) = self.next_backend_key();
        let session = SessionState {
            pid,
            cancel: self.cancels.register(pid, secret),
            // One index per session. Scoping it here rather than to the server
            // bounds how stale a cached walk can get to something the user
            // controls: reconnect and the disk is read again.
            files_cache: self.config.cache.then(|| Arc::new(FileIndexCache::new())),
        };

        println!(
            "zql: session pid={pid} user={} database={}",
            parameters.user(),
            parameters.database()
        );

        let result = self.handshake_and_loop(&mut reader, &mut writer, secret, &session);

        // Always, on every exit path — otherwise the map grows for the lifetime
        // of the process.
        self.cancels.unregister(pid);
        println!("zql: session pid={pid} ended");
        result
    }

    fn handshake_and_loop(
        &self,
        reader: &mut BufReader<TcpStream>,
        writer: &mut BufWriter<TcpStream>,
        secret: i32,
        session: &SessionState,
    ) -> io::Result<()> {
        send(writer, &backend::authentication_ok())?;
        for parameter in backend::startup_parameters(SERVER_VERSION) {
            send(writer, &parameter)?;
        }
        send(writer, &backend::backend_key_data(session.pid, secret))?;
        send(writer, &backend::ready_for_query(TransactionStatus::Idle))?;
        writer.flush()?;

        loop {
            let Some(message) = frontend::read(reader)? else {
                return Ok(()); // client vanished without saying goodbye
            };

            match message {
                Frontend::Terminate => return Ok(()),

                Frontend::Query(sql) => {
                    // Reset when a query *starts*. Resetting when a cancel is
                    // consumed instead would let a cancel that arrives just
                    // after a query ends poison the following one.
                    session.cancel.store(false, Ordering::SeqCst);
                    self.run_query(writer, &sql, session)?;
                }

                // A bare `Sync` closes an extended-protocol exchange. It is not
                // an error on its own; the correct reply is just readiness.
                Frontend::Sync => {}

                unsupported @ Frontend::Unsupported { .. } => {
                    let error = ZqlError::unsupported(unsupported.describe()).with_hint(
                        "zql implements the simple query protocol; \
                         psql and node-postgres use it by default",
                    );
                    send(writer, &backend::error_response(&error))?;

                    // **The extended protocol's error rule.** A client sends
                    // Parse/Bind/Describe/Execute/Sync as one batch without
                    // waiting for replies, so by the time the first message is
                    // refused the rest are already in flight. Postgres discards
                    // them and answers the whole batch with a single
                    // `ReadyForQuery`.
                    //
                    // Replying to each one instead — which is what zql did
                    // until node-postgres caught it — sends four errors and
                    // four readiness messages for one query. The client counts
                    // replies, finds extras it never asked for, and desynchronises.
                    if unsupported.is_extended_protocol() {
                        skip_until_sync(reader)?;
                    }
                }
            }

            send(writer, &backend::ready_for_query(TransactionStatus::Idle))?;
            writer.flush()?;
        }
    }

    /// Runs one query and streams its result.
    ///
    /// An engine error becomes an `ErrorResponse` and the session carries on;
    /// only an IO error — a client that has gone — ends the connection.
    fn run_query(
        &self,
        writer: &mut BufWriter<TcpStream>,
        sql: &str,
        session: &SessionState,
    ) -> io::Result<()> {
        let started = std::time::Instant::now();
        let before = self.cache_activity(session);

        let plan = match self.prepare(sql, session) {
            Ok(Prepared::Empty) => return send(writer, &backend::empty_query_response()),
            Ok(Prepared::Plan(plan)) => plan,
            Err(error) => {
                eprintln!("zql: {error}");
                self.report(sql, 0, started, before, session, Some(&error));
                return send(writer, &backend::error_response(&error));
            }
        };

        // The schema is settled before execution begins, which is the whole
        // reason binding is its own phase: this message has to go first.
        let schema = plan.schema().clone();
        send(writer, &backend::row_description(&schema))?;

        // Rows are streamed as they are produced rather than collected first.
        // That is what makes `LIMIT` over a huge scan feel instant, and what
        // will make an endless `tail()` work at all.
        let mut rows = 0u64;
        let mut stream = match exec::execute(plan, Arc::clone(&session.cancel)) {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("zql: {error}");
                return send(writer, &backend::error_response(&error));
            }
        };

        // Rows are buffered, but a buffer that is only flushed when the query
        // *ends* is invisible to a query that never ends: `tail()` would fill
        // 8 KB of kernel-side nothing while the user watched an empty screen.
        //
        // Flushing every row instead would cost a syscall each, which is real
        // money over a 127,000-row scan. Flushing on a short timer gets both:
        // bulk results still go out in full buffers, and a trickle of live rows
        // reaches the client within a frame.
        const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
        let mut last_flush = std::time::Instant::now();

        loop {
            match stream.next() {
                Ok(Some(row)) => {
                    send(writer, &backend::data_row(&row))?;
                    rows += 1;
                    if last_flush.elapsed() >= FLUSH_INTERVAL {
                        writer.flush()?;
                        last_flush = std::time::Instant::now();
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    // Mid-stream failure. `RowDescription` and some rows are
                    // already on the wire, which the protocol allows: an
                    // `ErrorResponse` may follow them, and the client discards
                    // the partial result.
                    eprintln!("zql: {error}");
                    self.report(sql, rows, started, before, session, Some(&error));
                    return send(writer, &backend::error_response(&error));
                }
            }
        }

        self.report(sql, rows, started, before, session, None);
        send(writer, &backend::command_complete(&backend::select_tag(rows)))
    }

    fn cache_activity(&self, session: &SessionState) -> CacheActivity {
        session
            .files_cache
            .as_ref()
            .map(|cache| cache.activity())
            .unwrap_or(CacheActivity { hits: 0, builds: 0 })
    }

    /// Publishes one query to the dashboard, if anyone is watching.
    fn report(
        &self,
        sql: &str,
        rows: u64,
        started: std::time::Instant,
        before: CacheActivity,
        session: &SessionState,
        error: Option<&ZqlError>,
    ) {
        let Some(dashboard) = &self.dashboard else {
            return;
        };
        let after = self.cache_activity(session);

        dash::publish(
            dashboard,
            &QueryEvent {
                sql,
                rows,
                millis: started.elapsed().as_millis(),
                error: error.map(|error| error.message.as_str()),
                index: CacheActivity::describe(before, after),
            },
        );
    }

    /// Parses and binds, producing something runnable.
    fn prepare(&self, sql: &str, session: &SessionState) -> Result<Prepared> {
        // Before `is_blank`, which tokenises: a catalogue query contains
        // operators zql's lexer does not have, so it would fail there first
        // with a message about generated SQL the user never wrote.
        catalogue_query_guard(sql)?;

        if sql::is_blank(sql)? {
            return Ok(Prepared::Empty);
        }

        match sql::parse(sql)? {
            Statement::Select(select) => {
                let config = SourceConfig {
                    dir: self.config.dir.clone(),
                    files_cache: session.files_cache.clone(),
                };
                Ok(Prepared::Plan(plan::bind(&select, &config)?))
            }
            // The discoverability answer to "what can I even query?", which is
            // the first thing anyone meeting this server wonders.
            Statement::ShowSources => Ok(Prepared::Plan(show_sources())),

            Statement::Explain(select) => {
                let config = SourceConfig {
                    dir: self.config.dir.clone(),
                    files_cache: session.files_cache.clone(),
                };
                let plan = plan::bind(&select, &config)?;
                Ok(Prepared::Plan(explain(&plan)))
            }
        }
    }

    /// A PID and secret key for `BackendKeyData`.
    ///
    /// There is no RNG in the standard library, so the secret is `SystemTime`
    /// nanoseconds mixed with the connection counter. This is stated plainly in
    /// the README rather than implying protection it does not have: it is
    /// guessable, and the worst a successful guess achieves is cancelling a
    /// query on a read-only server that binds loopback by default.
    fn next_backend_key(&self) -> (i32, i32) {
        let counter = self.next_pid.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos() as i32)
            .unwrap_or(0);

        // Keep the PID positive and plausible-looking; some clients log it.
        let pid = 4000 + counter.rem_euclid(60_000);
        let secret = nanos.wrapping_mul(2_654_435_761u32 as i32) ^ counter;
        (pid, secret)
    }
}

/// Discards the remainder of a failed extended-protocol batch.
///
/// Reads and drops messages until `Sync`, which is the message the client
/// always ends such a batch with. `Terminate` or a closed socket also end the
/// loop, so a client that gives up mid-batch does not strand the session.
fn skip_until_sync(reader: &mut BufReader<TcpStream>) -> io::Result<()> {
    loop {
        match frontend::read(reader)? {
            Some(Frontend::Sync) | Some(Frontend::Terminate) | None => return Ok(()),
            Some(_) => {}
        }
    }
}

/// Explains a `psql` backslash command rather than failing obscurely.
///
/// `\dt`, `\d` and `\l` are the first things anyone types at a Postgres prompt.
/// They are not protocol messages: `psql` expands each into a SQL query against
/// `pg_catalog`, which zql does not have. Without this, the user sees a lexer
/// error about an operator in generated SQL they never wrote — and concludes
/// the parser is broken rather than that the catalogue is absent.
fn catalogue_query_guard(sql: &str) -> Result<()> {
    const CATALOGUE_TABLES: &[&str] =
        &["pg_catalog", "pg_class", "pg_namespace", "pg_attribute", "pg_database"];

    let lowered = sql.to_ascii_lowercase();
    if !CATALOGUE_TABLES.iter().any(|table| lowered.contains(table)) {
        return Ok(());
    }

    Err(ZqlError::unsupported("the PostgreSQL catalogue tables")
        .with_detail(
            "psql expands its backslash commands into queries against pg_catalog, \
             which zql does not have",
        )
        .with_hint("run SHOW SOURCES to see what you can query"))
}

/// `SHOW SOURCES` — every source, its signature, and what it reads.
fn show_sources() -> Plan {
    let schema = Schema::new(vec![
        Column::new("source", Type::Text),
        Column::new("signature", Type::Text),
        Column::new("reads", Type::Text),
    ]);

    let rows = sources::CATALOGUE
        .iter()
        .map(|(name, signature, description)| {
            Row::new(vec![
                Value::Text((*name).to_string()),
                Value::Text((*signature).to_string()),
                Value::Text((*description).to_string()),
            ])
        })
        .collect();

    Plan::Values { schema, rows }
}

/// `EXPLAIN` — the plan tree, one operator per row.
fn explain(plan: &Plan) -> Plan {
    let schema = Schema::new(vec![Column::new("QUERY PLAN", Type::Text)]);
    let rows = plan
        .explain()
        .into_iter()
        .map(|line| Row::new(vec![Value::Text(line)]))
        .collect();

    Plan::Values { schema, rows }
}

/// State belonging to one connection.
///
/// Deliberately small and deliberately *not* on `Server`: everything here is
/// scoped to a single client, and a cache or a cancel flag shared across
/// connections would be a source of surprises rather than of speed.
struct SessionState {
    /// The fake PID advertised in `BackendKeyData`.
    pid: i32,
    cancel: CancelFlag,
    files_cache: Option<Arc<FileIndexCache>>,
}

/// A statement that is ready to run.
enum Prepared {
    /// The query was empty or a bare comment. `psql` sends one every time a
    /// user types a lone semicolon.
    Empty,
    Plan(Plan),
}

/// Pairs the read and write halves of a connection for the startup exchange,
/// which is the one place that has to read and write in the same call.
struct Duplex<'a> {
    reader: &'a mut BufReader<TcpStream>,
    writer: &'a mut BufWriter<TcpStream>,
}

impl io::Read for Duplex<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

impl io::Write for Duplex<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

fn send(writer: &mut impl Write, message: &Message) -> io::Result<()> {
    message.write_to(writer)
}

/// A client that closed its socket is an ordinary end to a session, not a
/// fault worth logging.
fn is_disconnect(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            // A handshake that never arrived. Ordinary for a port scanner or a
            // client that changed its mind, and not worth a log line.
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session() -> SessionState {
        SessionState {
            pid: 1,
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            files_cache: None,
        }
    }

    fn server() -> Server {
        Server::new(Config {
            host: "127.0.0.1".into(),
            port: 0,
            dir: PathBuf::from("."),
            cache: true,
            dashboard: None,
        })
    }

    #[test]
    fn a_blank_query_is_answered_with_an_empty_query_response() {
        for sql in ["", "  ;  ", "-- just a note"] {
            assert!(
                matches!(server().prepare(sql, &test_session()).unwrap(), Prepared::Empty),
                "{sql:?} should be blank"
            );
        }
    }

    #[test]
    fn a_select_with_no_from_binds_to_a_values_plan() {
        let plan = match server().prepare("SELECT 1 AS n", &test_session()).unwrap() {
            Prepared::Plan(plan) => plan,
            Prepared::Empty => panic!("expected a plan"),
        };
        assert_eq!(plan.schema().columns[0].name, "n");
    }

    #[test]
    fn an_unknown_source_is_undefined_table_and_lists_what_does_exist() {
        let Err(error) = server().prepare("SELECT * FROM nope", &test_session()) else {
            panic!("an unknown source must not bind");
        };
        assert_eq!(error.state, crate::error::SqlState::UndefinedTable);
        assert!(error.hint.unwrap().contains("files"));
    }

    /// Starts a real server on an ephemeral port and returns it.
    fn start_test_server() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind an ephemeral port");
        let port = listener.local_addr().expect("local addr").port();

        let server = Server::new(Config {
            host: "127.0.0.1".into(),
            port,
            dir: PathBuf::from("."),
            cache: true,
            dashboard: None,
        });
        thread::spawn(move || {
            let _ = server.serve_on(listener);
        });

        // Give the accept loop a moment to reach `incoming()`.
        thread::sleep(std::time::Duration::from_millis(150));
        port
    }

    /// A minimal v3 client: connect, hand over startup, read to ReadyForQuery.
    fn handshake(port: u16) -> TcpStream {
        use std::io::Read;

        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();

        stream.write_all(&8i32.to_be_bytes()).unwrap();
        stream.write_all(&80_877_103i32.to_be_bytes()).unwrap();
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"N", "SSLRequest must be refused with a bare N");

        let params = b"user\0zql\0database\0zql\0\0";
        stream
            .write_all(&((params.len() + 8) as i32).to_be_bytes())
            .unwrap();
        stream.write_all(&196_608i32.to_be_bytes()).unwrap();
        stream.write_all(params).unwrap();

        loop {
            let mut tag = [0u8; 1];
            stream.read_exact(&mut tag).unwrap();
            let mut len = [0u8; 4];
            stream.read_exact(&mut len).unwrap();
            let mut body = vec![0u8; (i32::from_be_bytes(len) - 4) as usize];
            stream.read_exact(&mut body).unwrap();
            if &tag == b"Z" {
                return stream;
            }
        }
    }

    fn send_query(stream: &mut TcpStream, sql: &str) {
        let mut payload = sql.as_bytes().to_vec();
        payload.push(0);
        stream.write_all(b"Q").unwrap();
        stream
            .write_all(&((payload.len() + 4) as i32).to_be_bytes())
            .unwrap();
        stream.write_all(&payload).unwrap();
    }

    /// Reads until ReadyForQuery, returning the message tags seen.
    fn read_reply(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
        use std::io::Read;

        let mut tags = Vec::new();
        loop {
            let mut tag = [0u8; 1];
            stream.read_exact(&mut tag)?;
            let mut len = [0u8; 4];
            stream.read_exact(&mut len)?;
            let mut body = vec![0u8; (i32::from_be_bytes(len) - 4) as usize];
            stream.read_exact(&mut body)?;
            tags.push(tag[0]);
            if &tag == b"Z" {
                return Ok(tags);
            }
        }
    }

    /// The `catch_unwind` boundary, exercised by a real panic on a real socket.
    ///
    /// `ARCHITECTURE.md` §5 calls this the last-resort net: one malformed
    /// database must not be able to end the server for every other client.
    /// Nothing in zql panics any more, so the panic is raised by a source that
    /// exists only under `cfg(test)` — the alternative being to assert the net
    /// works without ever dropping anything into it.
    ///
    /// This also pins `panic = "abort"` *out* of the release profile. Under
    /// abort, `catch_unwind` cannot catch anything and the first panic would
    /// take down every connected client; the assertion below would fail by the
    /// whole test process dying.
    #[test]
    fn a_panicking_connection_is_contained_and_the_server_survives() {
        let port = start_test_server();

        // A healthy client first, to prove the server was working beforehand.
        let mut before = handshake(port);
        send_query(&mut before, "SELECT 1");
        assert!(read_reply(&mut before).unwrap().contains(&b'D'), "no row before");

        // Now a connection whose query panics inside the connection thread.
        let mut doomed = handshake(port);
        send_query(&mut doomed, "SELECT * FROM __panic_probe");
        // The panic unwinds past the session, so this connection gets no reply
        // and is dropped. Either outcome is acceptable for *this* socket; what
        // matters is what happens to everything else.
        let _ = read_reply(&mut doomed);

        // The pre-existing session must be unharmed.
        send_query(&mut before, "SELECT 2");
        assert!(
            read_reply(&mut before).unwrap().contains(&b'D'),
            "an unrelated session died with the panicking one"
        );

        // And the listener must still accept new work.
        let mut after = handshake(port);
        send_query(&mut after, "SELECT 3");
        assert!(
            read_reply(&mut after).unwrap().contains(&b'D'),
            "the server stopped accepting connections after a panic"
        );
    }

    /// The registry must not leak the panicking session's entry.
    ///
    /// `unregister` runs after the query loop returns; a panic unwinds *past*
    /// that, so the cleanup has to survive unwinding or the map grows by one
    /// entry per panic for the lifetime of the process.
    #[test]
    fn a_panicking_connection_does_not_leak_its_cancel_registration() {
        let registry = Registry::new();
        let flag = registry.register(9001, 42);

        // Unwind through a scope that owns the registration.
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = &flag;
            panic!("boom");
        }));
        assert!(result.is_err(), "the panic should have been caught");

        // The entry is still there, which is exactly why `session` unregisters
        // on every exit path rather than relying on unwinding to do it.
        registry.unregister(9001);
        assert!(!registry.cancel(9001, 42), "the entry outlived unregister");
    }

/// A stack overflow is the one failure `catch_unwind` cannot contain.
    ///
    /// Unlike a panic, it aborts the process, so a single connection sending a
    /// deeply nested expression would take every other session and the listener
    /// with it. Measured before `MAX_EXPR_DEPTH` existed: `SELECT 1+1+…+1` at
    /// 1,750 terms killed the server outright, exit code `0xC00000FD`.
    ///
    /// This is the same shape as the `catch_unwind` test above and asserts the
    /// same thing — a bystander and the listener both live — because the whole
    /// point is that the guard has to hold for the failure that cannot be caught
    /// after the fact.
    #[test]
    fn a_deeply_nested_query_cannot_take_the_server_down() {
        let port = start_test_server();

        let mut bystander = handshake(port);
        send_query(&mut bystander, "SELECT 1");
        assert!(read_reply(&mut bystander).unwrap().contains(&b'D'), "no row before");

        // Far past the depth at which the binder used to exhaust its stack.
        let mut attacker = handshake(port);
        let deep = format!("SELECT {}1", "1+".repeat(5000));
        send_query(&mut attacker, &deep);
        let reply = read_reply(&mut attacker).expect("the query should be answered, not fatal");
        assert!(
            reply.contains(&b'E'),
            "a 5,000-deep expression should be refused with an error"
        );

        // The refusal must leave this connection usable, not merely alive.
        send_query(&mut attacker, "SELECT 1");
        assert!(
            read_reply(&mut attacker).unwrap().contains(&b'D'),
            "the refusing session desynchronised"
        );

        // And nothing else may have noticed.
        send_query(&mut bystander, "SELECT 2");
        assert!(
            read_reply(&mut bystander).unwrap().contains(&b'D'),
            "an unrelated session died with the deep query"
        );
        let mut after = handshake(port);
        send_query(&mut after, "SELECT 3");
        assert!(
            read_reply(&mut after).unwrap().contains(&b'D'),
            "the listener stopped accepting after the deep query"
        );
    }

    #[test]
    fn backend_keys_are_positive_and_distinct_per_session() {
        let server = Server::new(Config {
            host: "127.0.0.1".into(),
            port: 0,
            dir: PathBuf::from("."),
            cache: true,
            dashboard: None,
        });
        let (first_pid, _) = server.next_backend_key();
        let (second_pid, _) = server.next_backend_key();
        assert!(first_pid > 0 && second_pid > 0);
        assert_ne!(first_pid, second_pid);
    }
}
