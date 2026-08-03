//! Terminal UI (harness spec/TUI spec). Depends only on `arbe-runtime`'s
//! event/command contract, never on `arbe-core` or other harness crates
//! directly, and contains no agent decision logic (TUI spec §2).
//!
//! The chat/approval/status UI lands in Phase 6.

pub use arbe_runtime;
