//! Process observability domain: collection, joined store, columns, filters,
//! tree, history, actions, deep detail, watches and export. UI-agnostic.
pub mod actions;
pub mod collector;
pub mod columns;
pub mod detail;
pub mod export;
pub mod filter;
pub mod history;
pub mod model;
pub mod platform;
pub mod store;
pub mod tree;
pub mod watch;

pub use model::{ProcSnapshot, ProcessKey};
