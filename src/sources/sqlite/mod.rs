//! `sqlite('file.db', 'table')` — the headline source.
//!
//! Almost every application on a machine keeps its data in SQLite: browsers,
//! phones, notes apps, photo libraries. You have dozens of `.db` files and no
//! way to look inside any of them without installing something. This module is
//! why zql exists.
//!
//! It is read-only, and it never opens the file for writing.

pub mod record;
