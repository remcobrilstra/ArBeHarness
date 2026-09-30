//! Command-line arguments. Deliberately strict: an unknown flag or a stray
//! argument is an error, so a typo (or a program probing for a flag this
//! version doesn't have) fails fast instead of silently opening the TUI.

use std::path::PathBuf;

use arbe_tui::arbe_runtime::arbe_core::SessionId;

pub const USAGE: &str = "\
Usage: arbeharness [OPTIONS]
       arbeharness login [ACCOUNT] [--dev-home DIR]
       arbeharness logout [ACCOUNT] [--dev-home DIR]

Starts the interactive terminal UI, or with --print runs one prompt
without a UI and exits. `login` / `logout` sign this harness in to (or
out of) a model service account, for providers that use one instead of
an API key. ACCOUNT: grok. It can be left out while there is only one.

Session:
      --workdir <DIR>      Project directory the agent works in [default: current
                           directory, or the session's own when resuming]
      --resume <ID>        Continue a saved session
      --name <TITLE>       Name the session (default: its first message)
      --prompt <TEXT>      Send TEXT as the first message, then stay interactive

Headless:
      --headless           Serve JSON-RPC 2.0 on stdin/stdout (one message per line)
                           instead of starting the UI
      --print <TEXT>       Run TEXT as a single turn, print the answer, and exit
      --approve <POLICY>   With --print, for tool calls that need approval:
                           `none` denies them [default]; `reads` approves low-risk
                           ones (reading and searching files) and denies the rest;
                           `all` approves them all
      --output <FORMAT>    With --print: `text` prints the answer [default];
                           `json` prints every event, then the result, as JSON lines

Configuration:
      --profile <NAME>     Settings profile: coding, general, grok-subscription, or one
                           from a config file
      --mode <MODE>        Start in MODE: `default`, or `plan` (read-only until you
                           approve the model's plan); also applies to --resume
      --provider <ID>      Model provider: ollama, openai, anthropic, openai_compatible,
                           grok_subscription
      --model <ID>         Model to use
      --config <FILE>      Extra config file, applied after ~/.arbe/config/config.toml
                           with the same trust (repeatable)
      --dev-home <DIR>     Use DIR instead of ~/.arbe for the harness's own files
                           (development and testing)

  -h, --help               Print this help
  -V, --version            Print the version

Exit status with --print: 0 answered, 1 failed, 2 invalid arguments or
configuration, 3 stopped before answering (a loop limit), 130 interrupted.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovePolicy {
    #[default]
    None,
    Reads,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Interactive {
        prompt: Option<String>,
    },
    Print {
        prompt: String,
    },
    Headless,
    /// `login [ACCOUNT]`: `None` when no account was named.
    Login {
        account: Option<String>,
    },
    Logout {
        account: Option<String>,
    },
    Help,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    pub mode: Mode,
    pub workdir: Option<PathBuf>,
    pub dev_home: Option<PathBuf>,
    pub profile: Option<String>,
    /// `--mode`: the session mode (`plan`, ...), not to be confused with
    /// [`Mode`], how the program runs.
    pub session_mode: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub config_files: Vec<PathBuf>,
    pub resume: Option<SessionId>,
    pub name: Option<String>,
    pub approve: ApprovePolicy,
    pub output: OutputFormat,
}

/// Parses the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut args = args.into_iter();
    let mut cli = Cli {
        mode: Mode::Interactive { prompt: None },
        workdir: None,
        dev_home: None,
        profile: None,
        session_mode: None,
        provider: None,
        model: None,
        config_files: Vec::new(),
        resume: None,
        name: None,
        approve: ApprovePolicy::default(),
        output: OutputFormat::default(),
    };
    let mut prompt = None;
    let mut print = None;
    let mut headless = false;
    // `login` / `logout` and the account named after it.
    let mut command: Option<(String, Option<String>)> = None;
    let mut approve = None;
    let mut output = None;
    let mut seen = Vec::new();

    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag.to_string(), Some(value)),
            _ => (arg.clone(), None),
        };
        match flag.as_str() {
            "-h" | "--help" => {
                return Ok(Cli {
                    mode: Mode::Help,
                    ..cli
                });
            }
            "-V" | "--version" => {
                return Ok(Cli {
                    mode: Mode::Version,
                    ..cli
                });
            }
            _ => {}
        }
        if !flag.starts_with('-') {
            match &mut command {
                None if matches!(flag.as_str(), "login" | "logout") => {
                    command = Some((flag, None));
                    continue;
                }
                Some((name, _)) if matches!(flag.as_str(), "login" | "logout") => {
                    return Err(format!("{name} and {flag} can't be combined"));
                }
                Some((_, value @ None)) => {
                    *value = Some(flag);
                    continue;
                }
                _ => {}
            }
            return Err(format!("unexpected argument {arg:?}"));
        }
        let mut value = || -> Result<String, String> {
            match inline {
                Some(v) => Ok(v.to_string()),
                None => args.next().ok_or_else(|| format!("{flag} needs a value")),
            }
        };
        let repeatable = flag == "--config";
        if !repeatable && seen.contains(&flag) {
            return Err(format!("{flag} given more than once"));
        }
        match flag.as_str() {
            "--workdir" => cli.workdir = Some(value()?.into()),
            "--dev-home" => cli.dev_home = Some(value()?.into()),
            "--profile" => cli.profile = Some(value()?),
            "--mode" => cli.session_mode = Some(value()?),
            "--provider" => cli.provider = Some(value()?),
            "--model" => cli.model = Some(value()?),
            "--config" => cli.config_files.push(value()?.into()),
            "--name" => cli.name = Some(value()?),
            "--prompt" => prompt = Some(value()?),
            "--print" => print = Some(value()?),
            "--headless" => headless = true,
            "--resume" => {
                let raw = value()?;
                cli.resume = Some(
                    raw.parse()
                        .map_err(|_| format!("--resume: {raw:?} is not a session id"))?,
                );
            }
            "--approve" => {
                approve = Some(match value()?.as_str() {
                    "none" => ApprovePolicy::None,
                    "reads" => ApprovePolicy::Reads,
                    "all" => ApprovePolicy::All,
                    other => {
                        return Err(format!(
                            "--approve: expected `none`, `reads` or `all`, got {other:?}"
                        ));
                    }
                });
            }
            "--output" => {
                output = Some(match value()?.as_str() {
                    "text" => OutputFormat::Text,
                    "json" => OutputFormat::Json,
                    other => {
                        return Err(format!(
                            "--output: expected `text` or `json`, got {other:?}"
                        ));
                    }
                });
            }
            _ => return Err(format!("unknown option {flag}")),
        }
        seen.push(flag);
    }

    if let Some((command, value)) = command {
        if headless
            || prompt.is_some()
            || print.is_some()
            || cli.resume.is_some()
            || cli.name.is_some()
            || cli.profile.is_some()
            || cli.session_mode.is_some()
            || cli.provider.is_some()
            || cli.model.is_some()
            || !cli.config_files.is_empty()
            || cli.workdir.is_some()
            || approve.is_some()
            || output.is_some()
        {
            return Err(format!(
                "{command} only accepts an account name and --dev-home \
                 (it does not start a session)"
            ));
        }
        cli.mode = match command.as_str() {
            "login" => Mode::Login { account: value },
            _ => Mode::Logout { account: value },
        };
        return Ok(cli);
    }
    if headless {
        if prompt.is_some() || print.is_some() || cli.resume.is_some() || cli.name.is_some() {
            return Err(
                "--headless can't be combined with --prompt, --print, --resume or --name \
                 (sessions are opened through the protocol)"
                    .into(),
            );
        }
        if approve.is_some() || output.is_some() {
            return Err("--approve and --output only apply with --print".into());
        }
        cli.mode = Mode::Headless;
        return Ok(cli);
    }
    cli.mode = match (prompt, print) {
        (Some(_), Some(_)) => return Err("--prompt and --print can't be combined".into()),
        (_, Some(prompt)) => {
            if prompt.trim().is_empty() {
                return Err("--print needs a non-empty prompt".into());
            }
            Mode::Print { prompt }
        }
        (prompt, None) => {
            if approve.is_some() || output.is_some() {
                return Err("--approve and --output only apply with --print".into());
            }
            Mode::Interactive { prompt }
        }
    };
    cli.approve = approve.unwrap_or_default();
    cli.output = output.unwrap_or_default();
    Ok(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(args: &[&str]) -> Result<Cli, String> {
        parse(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn no_arguments_is_interactive() {
        let cli = parse_str(&[]).unwrap();
        assert_eq!(cli.mode, Mode::Interactive { prompt: None });
        assert!(cli.resume.is_none() && cli.config_files.is_empty());
    }

    #[test]
    fn help_and_version_win_wherever_they_appear() {
        assert_eq!(parse_str(&["--version"]).unwrap().mode, Mode::Version);
        assert_eq!(
            parse_str(&["--workdir", "x", "-V"]).unwrap().mode,
            Mode::Version
        );
        assert_eq!(parse_str(&["-h"]).unwrap().mode, Mode::Help);
    }

    #[test]
    fn values_come_as_separate_arguments_or_after_an_equals_sign() {
        let id = SessionId::new();
        let cli = parse_str(&[
            "--workdir=/repo",
            "--resume",
            &id.to_string(),
            "--name",
            "fix: a=b",
            "--config",
            "a.toml",
            "--config=b.toml",
            "--prompt",
            "hello",
            "--mode=plan",
        ])
        .unwrap();
        assert_eq!(cli.workdir, Some(PathBuf::from("/repo")));
        assert_eq!(cli.session_mode.as_deref(), Some("plan"));
        assert_eq!(cli.resume, Some(id));
        assert_eq!(cli.name.as_deref(), Some("fix: a=b"));
        assert_eq!(
            cli.config_files,
            [PathBuf::from("a.toml"), PathBuf::from("b.toml")]
        );
        assert_eq!(
            cli.mode,
            Mode::Interactive {
                prompt: Some("hello".into())
            }
        );
    }

    #[test]
    fn print_mode_takes_approval_and_output_settings() {
        let cli =
            parse_str(&["--print", "resolve it", "--approve", "all", "--output=json"]).unwrap();
        assert_eq!(
            cli.mode,
            Mode::Print {
                prompt: "resolve it".into()
            }
        );
        assert_eq!(cli.approve, ApprovePolicy::All);
        assert_eq!(cli.output, OutputFormat::Json);
        // Defaults: deny what needs approval, print text.
        let cli = parse_str(&["--print", "x"]).unwrap();
        assert_eq!(
            (cli.approve, cli.output),
            (ApprovePolicy::None, OutputFormat::Text)
        );
    }

    #[test]
    fn headless_is_its_own_mode() {
        let cli = parse_str(&["--headless", "--workdir", "/repo"]).unwrap();
        assert_eq!(cli.mode, Mode::Headless);
        assert_eq!(cli.workdir, Some(PathBuf::from("/repo")));
    }

    #[test]
    fn login_and_logout_are_commands() {
        let cli = parse_str(&["login", "--dev-home", "home"]).unwrap();
        assert_eq!(cli.mode, Mode::Login { account: None });
        assert_eq!(cli.dev_home, Some(PathBuf::from("home")));
        assert_eq!(
            parse_str(&["logout"]).unwrap().mode,
            Mode::Logout { account: None }
        );
        assert_eq!(
            parse_str(&["--dev-home", "home", "login", "grok"])
                .unwrap()
                .mode,
            Mode::Login {
                account: Some("grok".into())
            }
        );
        assert_eq!(
            parse_str(&["logout", "grok"]).unwrap().mode,
            Mode::Logout {
                account: Some("grok".into())
            }
        );
    }

    #[test]
    fn mistakes_are_errors_not_ignored() {
        for (args, expected) in [
            (&["--verison"][..], "unknown option --verison"),
            (&["hello"][..], "unexpected argument"),
            (&["--model"][..], "--model needs a value"),
            (&["--resume", "abc"][..], "not a session id"),
            (&["--model", "a", "--model", "b"][..], "more than once"),
            (&["--approve", "all"][..], "only apply with --print"),
            (
                &["--print", "x", "--approve", "some"][..],
                "expected `none`, `reads` or `all`",
            ),
            (&["--print", "x", "--prompt", "y"][..], "can't be combined"),
            (&["--print", " "][..], "non-empty"),
            (&["--headless", "--print", "x"][..], "can't be combined"),
            (
                &["login", "--print", "x"][..],
                "only accepts an account name",
            ),
            (&["login", "logout"][..], "can't be combined"),
            (&["login", "grok", "extra"][..], "unexpected argument"),
            (&["grok"][..], "unexpected argument"),
            (&["review"][..], "unexpected argument"),
        ] {
            let err = parse_str(args).unwrap_err();
            assert!(err.contains(expected), "{args:?}: {err}");
        }
    }
}
