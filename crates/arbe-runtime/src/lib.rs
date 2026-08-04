//! Orchestration glue: composes providers, memory, tools, skills, and hooks
//! into a running `Agent`, and exposes the event/command contract the TUI
//! (or any other client) consumes (overall design §3-4).

pub mod agent;
pub mod config;
pub mod event_bus;
pub mod system_prompt;

pub use agent::{Agent, ToolDecisions};
pub use config::RuntimeConfig;
pub use event_bus::EventBus;

pub use arbe_core;
pub use arbe_storage;
pub use arbe_tools;
