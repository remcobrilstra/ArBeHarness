//! Filesystem persistence rooted at `~/.arbe/` (harness spec §5, overall
//! design §6): session metadata, append-only turn/event logs, and atomic
//! writes for anything mutable.

pub mod atomic;
pub mod error;
pub mod memory_files;
pub mod paths;
pub mod session_store;

pub use error::StorageError;
pub use session_store::SessionStore;
