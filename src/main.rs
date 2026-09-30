mod cli;
mod headless;
mod print;
#[cfg(test)]
mod test_support;

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
        Mode::Login { .. } | Mode::Logout { .. } => {
            let home = cli
                .dev_home
                .clone()
                .unwrap_or_else(arbe_storage::paths::arbe_home);
            init_logging(&home);
            let runtime =
                tokio::runtime::Runtime::new().expect("failed to start the tokio runtime");
            let code = runtime.block_on(account_command(&cli.mode, &home));
            runtime.shutdown_background();
            std::process::exit(code);
        }
        _ => {}
    }

    let harness = build_harness(&cli).unwrap_or_else(|err| {
        eprintln!("error: {err}");
        std::process::exit(EXIT_USAGE);
    });
    init_logging(&harness.config().home);
    let runtime = tokio::runtime::Runtime::new().expect("failed to start the tokio runtime");
    let code = match cli.mode.clone() {
        Mode::Print { prompt } => runtime.block_on(print::run(&harness, &cli, prompt)),
        Mode::Headless => runtime.block_on(headless::run(harness)),
        Mode::Interactive { prompt } => runtime.block_on(interactive(harness, &cli, prompt)),
        Mode::Help | Mode::Version | Mode::Login { .. } | Mode::Logout { .. } => {
            unreachable!()
        }
    };
    // Don't wait for background tasks (e.g. MCP servers) to wind down.
    runtime.shutdown_background();
    std::process::exit(code);
}

/// The default log filter: the harness at `info`, chatty HTTP/TLS
/// libraries only at `warn`.
const DEFAULT_LOG_FILTER: &str =
    "info,hyper=warn,hyper_util=warn,reqwest=warn,h2=warn,rustls=warn,html5ever=warn";

/// Writes the harness's log to `<home>/logs/arbeharness.<date>.log`
/// (daily files, the last 14 kept) — never to the terminal, which the chat
/// screen, `--print` output and the JSON-RPC stream own. `ARBE_LOG` sets
/// the filter (e.g. `debug`, `arbe_runtime=trace`). If the directory can't
/// be written, the harness runs without a log.
fn init_logging(home: &std::path::Path) {
    use tracing_appender::rolling::{Builder, Rotation};
    use tracing_subscriber::EnvFilter;

    // Created first: otherwise the appender's pruning of old files reports
    // the missing directory on stderr, over the chat screen.
    let dir = home.join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(appender) = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("arbeharness")
        .filename_suffix("log")
        .max_log_files(14)
        .build(dir)
    else {
        return;
    };
    let filter = std::env::var("ARBE_LOG")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .and_then(|v| EnvFilter::try_new(v).ok())
        .unwrap_or_else(|| EnvFilter::new(DEFAULT_LOG_FILTER));
    let _ = tracing_subscriber::fmt()
        .with_writer(appender)
        .with_ansi(false)
        .with_env_filter(filter)
        .try_init();
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
    // Validated with the rest of the configuration.
    if let Some(mode) = &cli.session_mode {
        builder = builder.setting("ARBE_MODE", mode);
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

/// `login` and `logout` talk only to an account's credential file, under
/// `<home>/auth`. Exit status: 0 done, 1 the sign-in service or the file
/// failed, 2 no such account, 130 interrupted.
async fn account_command(mode: &Mode, home: &std::path::Path) -> i32 {
    use arbe_tui::arbe_runtime::arbe_core::ProviderError;
    use arbe_tui::arbe_runtime::arbe_providers::CancellationToken;
    use arbe_tui::arbe_runtime::arbe_providers::auth::{self, Account};

    let (Mode::Login { account: name } | Mode::Logout { account: name }) = mode else {
        unreachable!()
    };
    let scheme = match pick_scheme(auth::builtin_schemes(), name.as_deref()) {
        Ok(scheme) => scheme,
        Err(err) => {
            eprintln!("error: {err}");
            return EXIT_USAGE;
        }
    };
    let account = Account::new(scheme, &auth::auth_dir(home));
    let display_name = account.scheme().display_name.clone();
    let path = account.credential_path().display().to_string();

    if matches!(mode, Mode::Logout { .. }) {
        return match account.logout().await {
            Ok(true) => {
                eprintln!("Signed out of the {display_name}. Removed {path}.");
                0
            }
            Ok(false) => {
                eprintln!("Not signed in to the {display_name}; nothing to remove.");
                0
            }
            Err(err) => {
                eprintln!("error: {err}");
                1
            }
        };
    }

    let cancel = CancellationToken::new();
    let cancelling = cancel.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        cancelling.cancel();
    });
    let result = account
        .login(&auth::oauth_client(), &cancel, |prompt| {
            eprintln!("Sign in to the {display_name}.");
            eprintln!();
            if let Some(url) = &prompt.verification_uri_complete {
                eprintln!("Open this URL and confirm the code:");
                eprintln!("  {url}");
            } else {
                eprintln!("Open this URL:");
                eprintln!("  {}", prompt.verification_uri);
                eprintln!();
                eprintln!("Confirm this code: {}", prompt.user_code);
            }
            eprintln!();
            eprintln!("Waiting for authorization...");
        })
        .await;
    match result {
        Ok(()) => {
            eprintln!("Signed in to the {display_name}. Credential saved to {path}.");
            0
        }
        Err(ProviderError::Cancelled) => 130,
        Err(err) => {
            eprintln!("error: {err}");
            1
        }
    }
}

/// The account `login` / `logout` acts on: the one named, or the only one
/// there is when none is named.
fn pick_scheme(
    mut schemes: Vec<arbe_tui::arbe_runtime::arbe_providers::auth::AuthScheme>,
    name: Option<&str>,
) -> Result<arbe_tui::arbe_runtime::arbe_providers::auth::AuthScheme, String> {
    let known = || {
        schemes
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match name {
        Some(name) => match schemes.iter().position(|s| s.id == name) {
            Some(index) => Ok(schemes.swap_remove(index)),
            None => Err(format!("no account named {name:?} (known: {})", known())),
        },
        None if schemes.len() == 1 => Ok(schemes.remove(0)),
        None => Err(format!("name the account to use (known: {})", known())),
    }
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
    // A resumed session keeps its own mode unless --mode says otherwise.
    if let (Some(mode), Some(_)) = (&cli.session_mode, cli.resume)
        && let Err(err) = agent.set_mode(mode)
    {
        eprintln!("failed to set the mode: {err}");
    }

    let handle = tokio::runtime::Handle::current();
    let result =
        tokio::task::spawn_blocking(move || arbe_tui::run(harness, agent, handle, events, prompt))
            .await;
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(err)) => {
            eprintln!("TUI exited with an error: {err}");
            1
        }
        // The panic message was already printed (after the terminal was
        // restored, see `arbe_tui::run`).
        Err(_) => {
            eprintln!("the TUI stopped unexpectedly");
            101
        }
    }
}
