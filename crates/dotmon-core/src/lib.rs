//! `dotmon-core` — pure, OS-independent and UI-independent logic.
//!
//! Nothing in this crate talks to the kernel or draws pixels. Everything here is
//! deterministic and unit-tested; the system backends (`dotmon-sys`) produce
//! [`model::Snapshot`]s and the UI (`dotmon-app`) consumes the results.

pub mod actions;
pub mod apps;
pub mod chart;
pub mod format;
pub mod history;
pub mod model;
pub mod protection;
pub mod query;
pub mod settings;
pub mod source;

pub use model::{ProcessId, ProcessInfo, Snapshot};
