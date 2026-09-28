mod cli;
mod print;

use std::path::PathBuf;
use std::sync::Arc;

use arbe_tui::arbe_runtime::arbe_storage::{self, SessionStore};
use arbe_tui::arbe_runtime::{EventBus, Harness};
use cli::{Cli, Mode};

/// Exit status for invalid arguments or configuration.
const EXIT_USAGE: i32 = 2;

fn main() {
    let cli = match cli::parse(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(err) => {
            eprintln!("error: {err}\n\nRun `arbeharness --help` for usage.");
            std::process::exit(EXIT_USAGE);
        }
    };
    match cli.mode {
        Mode::Help => {
            println!("{}", cli::USAGE);
            return;
        }
        Mode::Version => {
            println!("arbeharness {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        _ => {}
    }

    let harness = build_harness(&cli).unwrap_or_else(|err| {
        eprintln!("error: {err}");
        std::process::exit(EXIT_USAGE);
    });
    let runtime = tokio::runtime::Runtime::new().expect("failed to start the tokio runtime");
    let code = match cli.mode.clone() {
        Mode::Print { prompt } => runtime.block_on(print::run(&harness, &cli, prompt)),
        Mode::Interactive { prompt } => runtime.block_on(interactive(harness, &cli, prompt)),
        Mode::Help | Mode::Version => unreachable!(),
    };
    // Don't wait for background tasks (e.g. MCP servers) to wind down.
    runtime.shutdown_background();
    std::process::exit(code);
}

/// Configuration layers: defaults < ~/.arbe/config/config.toml < each
/// `--config` file < <workdir>/.arbe/config.toml < the selected profile
/// < ARBE_* env vars < command-line flags.
fn build_harness(cli: &Cli) -> Result<Harness, String> {
    let mut builder = Harness::builder();
    let home = match &cli.dev_home {
        Some(dir) => {
            builder = builder.home(dir);
            dir.clone()
        }
        None => arbe_storage::paths::arbe_home(),
    };
    for file in &cli.config_files {
        if !file.is_file() {
            return Err(format!("--config: {} does not exist", file.display()));
        }
        builder = builder.config_file(file);
    }
    let workdir = match (&cli.workdir, cli.resume) {
        (Some(dir), _) => Some(dir.clone()),
        // Resuming without --workdir: continue where the session worked.
        (None, Some(id)) => {
            let meta = SessionStore::with_root(home.join("sessions"))
                .load_meta(id)
                .map_err(|e| format!("--resume {id}: {e}"))?;
            meta.workdir
        }
        (None, None) => None,
    };
    if let Some(dir) = workdir {
        builder = builder.project_dir(absolute(dir));
    }
    if let Some(profile) = &cli.profile {
        builder = builder.profile(profile);
    }
    if let Some(provider) = &cli.provider {
        builder = builder.provider(provider);
    }
    if let Some(model) = &cli.model {
        builder = builder.model(model);
    }
    builder
        .build()
        .map_err(|e| format!("invalid configuration: {e}"))
}

fn absolute(dir: PathBuf) -> PathBuf {
    std::path::absolute(&dir).unwrap_or(dir)
}

async fn interactive(harness: Harness, cli: &Cli, prompt: Option<String>) -> i32 {
    let events = Arc::new(EventBus::default());
    let agent = match cli.resume {
        Some(id) => harness.resume_agent(id, events.clone()),
        None => harness.create_agent(events.clone()),
    };
    let agent = match agent {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("failed to start ArBeHarness: {err}");
            return 1;
        }
    };
    if let Some(name) = &cli.name
        && let Err(err) = agent.set_title(name.clone())
    {
        eprintln!("failed to name the session: {err}");
    }

    let handle = tokio::runtime::Handle::current();
    let result =
        tokio::task::spawn_blocking(move || arbe_tui::run(harness, agent, handle, events, prompt))
            .await
            .expect("TUI task panicked");
    match result {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("TUI exited with an error: {err}");
            1
        }
    }
}
