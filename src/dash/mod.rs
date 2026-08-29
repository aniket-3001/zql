//! The live dashboard: a hand-written HTTP/1.1 server and an SSE stream.
//!
//! Stands in for `hyper` or `tiny_http`. It serves exactly two things — one
//! page and one event stream — and is small enough that a framework would be
//! more code than the thing it replaced.

pub mod sse;
