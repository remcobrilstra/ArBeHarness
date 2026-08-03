use std::sync::Arc;

use arbe_tui::arbe_runtime::{Agent, EventBus, RuntimeConfig};

/// Reads `<flag> <value>` / `<flag>=<value>` from argv, if present.
fn parse_arg(flag: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    let with_eq = format!("{flag}=");
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix(&with_eq) {
            return Some(value.to_string());
        }
        if arg == flag {
            return args.next();
        }
    }
    None
}

fn main() {
    // Set ARBE_HOME/ARBE_WORKDIR (if overridden via CLI flags) before the
    // tokio runtime — and the worker threads that come with it — is
    // created, so these mutations can never race with another thread
    // reading/writing the env.
    //
    // --dev-home: **development/testing only.** Relocates the harness's
    // own storage root (~/.arbe/ - sessions, skills, memory, mcp config,
    // logs) to a scratch directory. Not something a normal run needs.
    if let Some(dev_home) = parse_arg("--dev-home") {
        unsafe {
            std::env::set_var("ARBE_HOME", dev_home);
        }
    }
    // --workdir: the project/repo directory the agent actually works on.
    // Defaults to the current directory if not given.
    if let Some(workdir) = parse_arg("--workdir") {
        unsafe {
            std::env::set_var("ARBE_WORKDIR", workdir);
        }
    }

    tokio::runtime::Runtime::new()
        .expect("failed to start the tokio runtime")
        .block_on(run());
}

async fn run() {
    let config = RuntimeConfig::from_env();
    let store = arbe_tui::arbe_runtime::arbe_storage::SessionStore::new();
    let events = Arc::new(EventBus::default());

    // Builtin tools (read_file/write_file/edit_file/list_dir/glob/grep/
    // execute) are registered automatically by Agent::create, sandboxed to
    // config.project_dir — see arbe_tools::builtin.
    let agent = match Agent::create(&config, store, events) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("failed to start ArBeHarness: {err}");
            std::process::exit(1);
        }
    };

    let handle = tokio::runtime::Handle::current();
    let result = tokio::task::spawn_blocking(move || arbe_tui::run(agent, handle))
        .await
        .expect("TUI task panicked");

    if let Err(err) = result {
        eprintln!("TUI exited with an error: {err}");
        std::process::exit(1);
    }
}
