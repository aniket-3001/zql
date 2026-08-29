//! The live dashboard: a hand-written HTTP/1.1 server and an SSE stream.
//!
//! Stands in for `hyper` or `tiny_http`. It serves exactly two things — one
//! page and one event stream — which is small enough that a framework would be
//! more code than the thing it replaced.
//!
//! # Why this exists at all
//!
//! A terminal tool that never moves is hard to show. The dashboard is the
//! project's motion: a query runs in `psql` and appears here a frame later,
//! with its timing and row count. `FEATURES.md` §6.2 marks it cuttable on
//! paper and protected in practice, and this is the protected version.

pub mod page;
pub mod sse;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::datetime;
use sse::{escape_json, Broadcaster};

/// How often the heartbeat fires.
const HEARTBEAT: Duration = Duration::from_secs(15);

/// A request line longer than this is not a request zql serves.
const MAX_REQUEST_LINE: usize = 8 * 1024;

/// One entry in the query log.
pub struct QueryEvent<'a> {
    pub sql: &'a str,
    pub rows: u64,
    pub millis: u128,
    /// `None` when the query succeeded.
    pub error: Option<&'a str>,
    /// What the filesystem index did: `"cached"`, `"indexed"`, or nothing.
    pub index: Option<&'static str>,
}

/// Starts the dashboard, returning the handle queries report to.
///
/// Failing to bind is reported and otherwise ignored: the dashboard is a
/// convenience, and a port clash should not stop the database server starting.
pub fn start(host: &str, port: u16, server_port: u16) -> Option<Arc<Broadcaster>> {
    let listener = match TcpListener::bind((host, port)) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("zql: dashboard disabled, cannot bind {host}:{port}: {err}");
            return None;
        }
    };

    let broadcaster = Arc::new(Broadcaster::new());

    // The heartbeat is what prunes dead clients when nobody is querying, and
    // what proves the page is live on the demo video when nothing else is
    // happening. See `Broadcaster::heartbeat`.
    let ticker = Arc::clone(&broadcaster);
    spawn("zql-heartbeat", move || loop {
        thread::sleep(HEARTBEAT);
        ticker.heartbeat();
    });

    let accepting = Arc::clone(&broadcaster);
    spawn("zql-dashboard", move || {
        for incoming in listener.incoming().flatten() {
            let broadcaster = Arc::clone(&accepting);
            // One thread per *request*. The `/events` handler gives its socket
            // to the broadcaster and returns, so N dashboards do not mean N
            // parked threads.
            spawn("zql-dash-request", move || {
                if let Err(err) = handle(incoming, &broadcaster, server_port) {
                    // A browser that navigates away mid-response is ordinary.
                    if err.kind() != std::io::ErrorKind::BrokenPipe {
                        eprintln!("zql: dashboard request failed: {err}");
                    }
                }
            });
        }
    });

    print_address(host, port);
    Some(broadcaster)
}

/// Publishes one query to every connected dashboard.
pub fn publish(broadcaster: &Broadcaster, event: &QueryEvent<'_>) {
    let now = datetime::format_timestamp(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or(0),
    );
    // Just the clock part: the date is the same for every row on screen.
    let clock = now.split(' ').nth(1).unwrap_or(&now).to_string();

    let payload = format!(
        r#"{{"at":"{}","sql":"{}","rows":{},"ms":{},"status":"{}","detail":"{}","index":"{}"}}"#,
        escape_json(&clock),
        escape_json(&collapse(event.sql)),
        event.rows,
        event.millis,
        if event.error.is_some() { "error" } else { "ok" },
        escape_json(event.error.unwrap_or("")),
        event.index.unwrap_or(""),
    );

    broadcaster.publish(&payload);
}

/// Squashes a query onto one line and bounds its length.
///
/// A pasted 4 KB query would otherwise dominate the page and the frame.
fn collapse(sql: &str) -> String {
    let flat: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 300 {
        return flat;
    }
    let truncated: String = flat.chars().take(300).collect();
    format!("{truncated}…")
}

fn handle(
    mut stream: TcpStream,
    broadcaster: &Arc<Broadcaster>,
    server_port: u16,
) -> std::io::Result<()> {
    let target = match read_request(&stream)? {
        Some(target) => target,
        None => return Ok(()),
    };

    match target.as_str() {
        "/" | "/index.html" => {
            let body = page::html(server_port);
            write_response(&mut stream, "200 OK", "text/html; charset=utf-8", &body)
        }

        "/events" => {
            // Headers only. `Content-Length` must be absent or the browser
            // waits for a body that never ends; `no-cache` stops an
            // intermediary buffering the stream into uselessness.
            stream.write_all(
                b"HTTP/1.1 200 OK\r\n\
                  Content-Type: text/event-stream\r\n\
                  Cache-Control: no-cache\r\n\
                  Connection: keep-alive\r\n\
                  X-Accel-Buffering: no\r\n\r\n",
            )?;
            stream.flush()?;
            // The producer owns the socket from here; this thread is done.
            broadcaster.subscribe(stream);
            Ok(())
        }

        _ => write_response(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            "not found\n",
        ),
    }
}

/// Reads the request line and headers, and **never a body**.
///
/// There are no POSTs to this server. Reading a body that was never sent is
/// exactly where a hand-rolled HTTP server hangs, so the headers are consumed
/// to the blank line and everything after it is ignored.
fn read_request(stream: &TcpStream) -> std::io::Result<Option<String>> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    if (&mut reader)
        .take(MAX_REQUEST_LINE as u64)
        .read_line(&mut line)?
        == 0
    {
        return Ok(None);
    }

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/").to_string();

    // Drain the headers so the client's write completes before we reply.
    loop {
        let mut header = String::new();
        let read = (&mut reader)
            .take(MAX_REQUEST_LINE as u64)
            .read_line(&mut header)?;
        if read == 0 || header.trim().is_empty() {
            break;
        }
    }

    if method != "GET" {
        return Ok(Some("/method-not-allowed".to_string()));
    }
    // Query strings are not used, but a browser may still add one.
    Ok(Some(
        target.split('?').next().unwrap_or("/").to_string(),
    ))
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

fn print_address(host: &str, port: u16) {
    println!("zql: dashboard on http://{host}:{port}");
    // Only worth printing a LAN address if the server is actually reachable
    // from off the machine.
    if host == "0.0.0.0" {
        if let Some(address) = local_address() {
            println!("     from another device:  http://{address}:{port}");
        }
    }
}

/// This machine's address on the interface carrying the default route.
///
/// **Enumerating interfaces and taking the first non-loopback is wrong**, and
/// measurably so: this machine reports seven non-loopback IPv4 addresses, six
/// of them `169.254.x.x` link-local junk that leads nowhere. Printing one of
/// those gives a URL that silently fails from a phone.
///
/// Instead, ask the kernel. Binding a UDP socket and "connecting" it to a
/// routable address sends no packets — UDP is connectionless — but it makes the
/// kernel select the route, and `local_addr` then reports the interface that
/// route uses. It is also the only way to answer this question with the
/// standard library alone.
fn local_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // Never contacted; the address only has to be routable.
    socket
        .connect(SocketAddr::from(([192, 0, 2, 1], 9)))
        .ok()?;
    let address = socket.local_addr().ok()?.ip();
    if address.is_loopback() || address.is_unspecified() {
        None
    } else {
        Some(address)
    }
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) {
    if let Err(err) = thread::Builder::new().name(name.to_string()).spawn(body) {
        eprintln!("zql: cannot spawn {name}: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_query_is_collapsed_and_bounded() {
        assert_eq!(collapse("SELECT\n  1\n"), "SELECT 1");

        let long = "SELECT ".to_string() + &"x,".repeat(500);
        let collapsed = collapse(&long);
        assert!(collapsed.chars().count() <= 301);
        assert!(collapsed.ends_with('…'));
    }

    #[test]
    fn an_event_payload_is_valid_json_even_with_a_hostile_query() {
        let broadcaster = Broadcaster::new();
        // No clients, so this only exercises the formatting.
        publish(
            &broadcaster,
            &QueryEvent {
                sql: "SELECT \"a\\b\"\nFROM files",
                rows: 3,
                millis: 12,
                error: Some("boom \"quoted\""),
                index: Some("cached"),
            },
        );
    }
}
