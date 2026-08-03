//! Filesystem persistence rooted at `~/.arbe/` (harness spec §5, overall
//! design §6). Session read/write, atomic writes, and JSONL append land in
//! Phase 1; this crate currently defines only path resolution and errors.

pub mod error;
pub mod paths;

pub use error::StorageError;
