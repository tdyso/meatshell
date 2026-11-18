//! Facade over the upstream `src/config` module tree.
//!
//! Mirrors upstream `src/config/mod.rs`, but with `pub` re-exports (upstream
//! uses `pub(crate)` because the desktop main.rs shares the crate; this crate
//! is a standalone library consumed by the OHOS UI layer). Every `#[path]`
//! target is upstream source, byte-for-byte unchanged.
//!
//! Layout expected by the upstream files (must not change):
//! - `impls/config.rs` does `use super::structs::*;`
//! - `impls/finalshell.rs` does `use super::structs::{...};`
//! - `impls/import.rs` (child of `config`) does `use super::{...}` and
//!   `super::super::{jump_chain, finalshell}`
//! - `jump_chain.rs` does `use super::{ConfigStore, Session, SessionKind}`
//! - struct leaf files do `use super::{sibling type names}` (via
//!   `config_structs.rs` glob re-exports)
//!
//! Keep this list in sync when upstream reorganizes `src/config/`.

#[path = "config_structs.rs"]
pub mod structs;
#[path = "../../../src/config/impls/config.rs"]
pub mod config;
#[path = "../../../src/config/impls/finalshell.rs"]
pub mod finalshell;
#[path = "../../../src/config/jump_chain.rs"]
pub mod jump_chain;

pub use config::*;
pub use finalshell::*;
pub use structs::*;
// Note: upstream `mod jump_chain` items (draft_jump_ids) are pub(crate); the
// UI layer does not need them.
