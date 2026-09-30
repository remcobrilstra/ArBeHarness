//! Running a command line the way a terminal would, on every platform:
//! `sh -c` on Unix, `cmd /S /C` on Windows. The one place this is decided,
//! for `execute`, command hooks, `provider.api_key_command` and MCP servers
//! started through the shell.

use std::process::Command;

/// A command that runs `line` in the platform shell (pipes, redirection,
/// `&&`, quoting all work as at a prompt). Returns a `std` command; async
/// callers convert it with `tokio::process::Command::from`.
///
/// On Windows the line is handed to `cmd` verbatim (`raw_arg`): Rust's
/// normal argument quoting escapes inner quotes with backslashes, which
/// `cmd` doesn't understand, so `git commit -m "fix bug"` would arrive
/// mangled.
pub fn command(line: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("cmd");
        c.raw_arg(format!("/S /C \"{line}\""));
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(line);
        c
    }
}

/// `program` and `args` as one command line for [`command`]: arguments
/// with spaces, quotes or shell characters are double-quoted (inner quotes
/// escaped), everything else is left as it is.
pub fn join(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(quote)
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./\\:=@+,%".contains(c));
    if plain {
        arg.to_string()
    } else {
        format!("\"{}\"", arg.replace('"', "\\\""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_that_need_it_are_quoted() {
        assert_eq!(
            join("npx", &["-y".into(), "@scope/server-x".into()]),
            "npx -y @scope/server-x"
        );
        assert_eq!(
            join("run", &["two words".into(), "say \"hi\"".into(), "".into()]),
            r#"run "two words" "say \"hi\"" """#
        );
        assert_eq!(join("run", &["a&b".into()]), r#"run "a&b""#);
    }

    #[test]
    fn a_command_line_runs_in_the_shell() {
        let output = command("echo one && echo two").output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("one") && text.contains("two"), "{text}");
    }
}
