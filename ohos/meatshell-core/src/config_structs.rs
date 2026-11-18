//! Public re-export of the upstream session/config struct definitions.
//!
//! Mirrors upstream `src/config/struct/mod.rs`, but with `pub` visibility:
//! upstream re-exports are `pub(crate)` because the desktop main.rs shares
//! the crate, while this crate is a standalone library consumed by the OHOS
//! UI layer. Every listed file is upstream source, byte-for-byte unchanged.
//! Keep this list in sync when upstream adds files under `src/config/struct/`.

#[path = "../../../src/config/struct/config_file.rs"]
pub mod config_file;
#[path = "../../../src/config/struct/config_store.rs"]
pub mod config_store;
#[path = "../../../src/config/struct/dock_stack.rs"]
pub mod dock_stack;
#[path = "../../../src/config/struct/quick_command.rs"]
pub mod quick_command;
#[path = "../../../src/config/struct/secret.rs"]
pub mod secret;
#[path = "../../../src/config/struct/session.rs"]
pub mod session;
#[path = "../../../src/config/struct/sftp_places.rs"]
pub mod sftp_places;

pub use config_file::*;
pub use config_store::*;
pub use dock_stack::*;
pub use quick_command::*;
pub use secret::*;
pub use session::*;
pub use sftp_places::*;
