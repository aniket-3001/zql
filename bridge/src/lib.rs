//! The playground's bridge: zql's engine, callable from JavaScript.
//!
//! # What this is, and what it is not
//!
//! This is not a reimplementation of anything. It calls the same `sql::parse`,
//! `plan::bind` and `exec::execute` the server calls, against the same sources,
//! and renders the result the same way `wire::oid` renders it for the socket.
//! What the page shows is the shipped engine running, not a description of it.
//!
//! It is compiled for `wasm32-wasip1` rather than `wasm32-unknown-unknown`
//! specifically so `std::fs` works. Under `unknown` the engine still parses,
//! binds and evaluates — but `sqlite()`, `csv()` and `files()` all fail at the
//! first read, which would leave the headline feature undemonstrable. With WASI
//! and a preloaded in-memory filesystem, the SQLite reader walks a real
//! b-tree in a real database file in the browser.
//!
//! # The interface
//!
//! No `wasm-bindgen`, and that is a deliberate cost. Hand-rolling the boundary
//! means one exported allocator and a length-prefixed byte buffer in each
//! direction — about forty lines — against a build-time dependency in a project
//! whose entire claim is not having any. The same argument the rest of the
//! repository makes, applied to the demo of it.
//!
//! JavaScript calls `alloc`, writes UTF-8 in, calls `query`, and reads a
//! length-prefixed UTF-8 JSON reply back out of the module's linear memory.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use zql::error::ZqlError;
use zql::exec;
use zql::plan::plan::Plan;
use zql::plan::schema::Schema;
use zql::sources::SourceConfig;
use zql::sql::ast::Statement;
use zql::value::{Row, Value};
use zql::wire::oid;

/// Rows returned to the page before it stops asking.
///
/// The page is a demo, not a terminal: nobody scrolls a hundred thousand rows
/// in a browser, and serialising them would cost more than producing them. The
/// reply says when it truncated so the page can say so too.
const MAX_ROWS: usize = 500;

// ---------------------------------------------------------------- allocation

/// Hands JavaScript a buffer to write into.
///
/// # Safety
///
/// The caller must pass the same length back to [`dealloc`], or free the
/// pointer by handing it to a function documented to take ownership. Called
/// only from the page's own glue, which does exactly that.
#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(len);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Releases a buffer obtained from [`alloc`].
///
/// # Safety
///
/// `ptr` must have come from `alloc(len)` and must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len != 0 {
        drop(Vec::from_raw_parts(ptr, 0, len));
    }
}

/// Runs one query and returns a length-prefixed UTF-8 JSON reply.
///
/// The reply is `[u32 little-endian length][bytes]`, allocated here and owned
/// by the caller, which frees it with [`dealloc`] once it has copied the bytes
/// out of linear memory.
///
/// # Safety
///
/// `ptr`/`len` must describe a valid UTF-8 buffer obtained from [`alloc`].
/// Ownership of that buffer stays with the caller.
#[no_mangle]
pub unsafe extern "C" fn query(ptr: *const u8, len: usize) -> *mut u8 {
    let sql = std::slice::from_raw_parts(ptr, len);
    let sql = String::from_utf8_lossy(sql).into_owned();

    // The engine is written never to panic and is swept for it in the test
    // suite, so this should never fire. It is here because a panic in wasm
    // aborts the module and every later query on the page fails with a
    // uselessly generic error — the page would look broken rather than report
    // a bug. Same reasoning as the `catch_unwind` around a connection.
    let reply = match std::panic::catch_unwind(|| run(&sql)) {
        Ok(json) => json,
        Err(_) => {
            r#"{"kind":"error","code":"XX000","message":"the engine panicked"}"#.to_string()
        }
    };

    let bytes = reply.into_bytes();
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&bytes);

    let p = out.as_mut_ptr();
    std::mem::forget(out);
    p
}

// -------------------------------------------------------------------- engine

fn run(sql: &str) -> String {
    let started = now_micros();

    if zql::sql::is_blank(sql).unwrap_or(false) {
        return format!(
            r#"{{"kind":"empty","micros":{}}}"#,
            now_micros().saturating_sub(started)
        );
    }

    let config = SourceConfig::uncached(PathBuf::from("/demo"));

    let statement = match zql::sql::parse(sql) {
        Ok(statement) => statement,
        Err(error) => return render_error(&error),
    };

    // `EXPLAIN` and `SHOW SOURCES` are answered exactly as the session loop
    // answers them, rather than being special-cased into something prettier for
    // the page. A demo that improves on the product is not a demo of it.
    let plan = match statement {
        Statement::Select(select) => match zql::plan::bind(&select, &config) {
            Ok(plan) => plan,
            Err(error) => return render_error(&error),
        },
        Statement::Explain(select) => match zql::plan::bind(&select, &config) {
            Ok(plan) => explain(&plan),
            Err(error) => return render_error(&error),
        },
        Statement::ShowSources => show_sources(),
    };

    // The plan tree is captured before execution consumes it, so the page can
    // show what the binder decided alongside what the query returned.
    let tree = plan.explain();
    let schema = plan.schema().clone();

    let cancel: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let mut stream = match exec::execute(plan, Arc::clone(&cancel)) {
        Ok(stream) => stream,
        Err(error) => return render_error(&error),
    };

    let mut rows: Vec<Row> = Vec::new();
    let mut truncated = false;
    loop {
        match stream.next() {
            Ok(Some(row)) => {
                if rows.len() >= MAX_ROWS {
                    truncated = true;
                    break;
                }
                rows.push(row);
            }
            Ok(None) => break,
            Err(error) => return render_error(&error),
        }
    }

    render_rows(&schema, &rows, truncated, &tree, now_micros().saturating_sub(started))
}

/// `EXPLAIN`, as `server::explain` builds it.
fn explain(plan: &Plan) -> Plan {
    use zql::plan::schema::Column;
    use zql::value::Type;

    Plan::Values {
        schema: Schema::new(vec![Column::new("QUERY PLAN", Type::Text)]),
        rows: plan
            .explain()
            .into_iter()
            .map(|line| Row::new(vec![Value::Text(line)]))
            .collect(),
    }
}

/// `SHOW SOURCES`, from the same catalogue the server reads.
fn show_sources() -> Plan {
    use zql::plan::schema::Column;
    use zql::value::Type;

    Plan::Values {
        schema: Schema::new(vec![
            Column::new("source", Type::Text),
            Column::new("signature", Type::Text),
            Column::new("reads", Type::Text),
        ]),
        rows: zql::sources::CATALOGUE
            .iter()
            .map(|(name, signature, description)| {
                Row::new(vec![
                    Value::Text((*name).to_string()),
                    Value::Text((*signature).to_string()),
                    Value::Text((*description).to_string()),
                ])
            })
            .collect(),
    }
}

// ------------------------------------------------------------------ rendering

fn render_rows(
    schema: &Schema,
    rows: &[Row],
    truncated: bool,
    tree: &[String],
    micros: u64,
) -> String {
    let mut out = String::from(r#"{"kind":"rows","columns":["#);

    for (i, column) in schema.columns.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        // The advertised OID and width are the ones that would go out in
        // `RowDescription`, so the page can show the wire types a real client
        // would receive rather than a guess made for display.
        out.push_str(&format!(
            r#"{{"name":{},"type":{},"oid":{},"width":{}}}"#,
            json_string(&column.name),
            json_string(column.ty.name()),
            oid::oid_for(column.ty),
            oid::type_size(column.ty),
        ));
    }

    out.push_str(r#"],"rows":["#);
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        for (j, value) in row.0.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            // `oid::render` is what writes a `DataRow` field, so NULL really is
            // absent here rather than an empty string — the page can draw the
            // distinction because the engine still makes it.
            match oid::render(value) {
                None => out.push_str("null"),
                Some(bytes) => {
                    out.push_str(&json_string(&String::from_utf8_lossy(&bytes)));
                }
            }
        }
        out.push(']');
    }

    out.push_str(r#"],"plan":["#);
    for (i, line) in tree.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_string(line));
    }

    out.push_str(&format!(
        r#"],"tag":{},"truncated":{},"micros":{}}}"#,
        json_string(&format!("SELECT {}", rows.len())),
        truncated,
        micros
    ));
    out
}

/// An error, with every field the wire protocol would carry.
///
/// The page draws a caret from `position` exactly as `psql` does, which is only
/// possible because the engine populates it — so the demo shows a real property
/// of the error type rather than a nicety added for the browser.
fn render_error(error: &ZqlError) -> String {
    let mut out = format!(
        r#"{{"kind":"error","code":{},"message":{}"#,
        json_string(error.state.code()),
        json_string(&error.message)
    );
    if let Some(detail) = &error.detail {
        out.push_str(&format!(r#","detail":{}"#, json_string(detail)));
    }
    if let Some(hint) = &error.hint {
        out.push_str(&format!(r#","hint":{}"#, json_string(hint)));
    }
    if let Some(position) = error.position {
        out.push_str(&format!(r#","position":{position}"#));
    }
    out.push('}');
    out
}

/// JSON string escaping.
///
/// zql has one of these in `dash::sse` for the event stream, but it is not
/// public and duplicating eight lines beats widening the shipped crate's API to
/// serve a demo.
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Microseconds since the epoch, for the timing the page reports.
///
/// WASI gives a real monotonic clock, so this measures the engine rather than
/// the round trip through JavaScript.
fn now_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}
