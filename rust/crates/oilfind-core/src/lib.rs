//! Core of Oil Find for Windows: metadata indexing, query matching and caching.
//!
//! This crate is a behaviour-compatible port of the Go implementation:
//!
//! * [`scan`] enumerates metadata under explicitly validated local roots. It
//!   never reads file contents, never follows links or reparse points, and
//!   returns all-or-nothing (cancelling yields [`Error::Cancelled`], never a
//!   partial index).
//! * [`Index::search`] compiles the query grammar (AND / OR / NOT / quotes /
//!   globs / `ext:` / `kind:` / `path:` / `size:` / `dm:` / `regex:`) and keeps
//!   only the best `limit` matches in a bounded heap.
//! * [`Index::save`] and [`Index::load`] persist the index atomically and
//!   stream-validate it on the way back in.
//!
//! The only environment access is the file system: no network, no shell, no
//! file contents.

mod cache;
mod entry;
mod error;
mod platform;
mod scan;
mod search;
mod time;

pub use cache::MAX_CACHE_BYTES;
pub use entry::{Entry, Index, Progress};
pub use error::Error;
pub use scan::{scan, MAX_ENTRIES, MAX_ROOTS};
