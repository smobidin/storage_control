//! storage-control: NAS console for Immich, Nextcloud and Stalwart.
//!
//! Rust rewrite of the Flask app; see `doc/rust-port.md` for the port plan and
//! the behavioural invariants the update runners must keep.

pub mod collect;
pub mod config;
pub mod db;
pub mod error;
pub mod exec;
pub mod importer;
pub mod runners;
pub mod sched;
pub mod updates;
pub mod security;
pub mod templates;
pub mod web;
