//! The server: connection lifecycle, the startup exchange, and cancellation.
//!
//! One `std::thread` per connection, blocking IO, no async runtime. A demo
//! server has single-digit clients; a thread pool here would be complexity
//! bought with nothing.

pub mod startup;
