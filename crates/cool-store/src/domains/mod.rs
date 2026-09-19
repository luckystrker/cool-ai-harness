//! Typed accessors for the legacy SQLite tables, grouped by subsystem.
//!
//! Every module implements methods on [`crate::LegacyStore`] and scopes rows by
//! actor through the `rust_actors` mapping. Reads validate conversation/run
//! ownership; writes update `updated_at` in the SQLAlchemy datetime format.
//!
//! Two rules for adding a domain module:
//!
//! 1. Never call another `&self` method while holding
//!    `self.connection()` — the connection mutex is not reentrant. Use the
//!    free functions in [`common`] with the held guard instead.
//! 2. JSON columns are text in SQLite; use [`common::parse_json`] and
//!    [`common::json_text`] so `NULL`/`{}` conventions stay compatible with
//!    the Python runtime.

pub mod analytics;
pub mod artifacts;
pub mod budgets;
pub(crate) mod common;
pub mod constructor;
pub mod conversations;
pub mod memory;
pub mod plans;
pub mod profiles;
pub mod providers;
pub mod research;
pub mod rss;
pub mod runs;
pub mod subagents;
pub mod tasks;
pub mod webhooks;
pub mod wiki;
