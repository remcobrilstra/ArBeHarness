//! Orchestration glue: composes providers, memory, tools, skills, hooks,
//! and MCP into the running agent loop, and exposes the event/command
//! contract the TUI (or any other client) consumes (overall design §3-4).
//! The loop state machine itself lands in Phase 1.

pub mod event_bus;

pub use arbe_core;
pub use event_bus::EventBus;
