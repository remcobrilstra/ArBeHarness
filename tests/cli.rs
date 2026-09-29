//! Drives the real `arbeharness` binary: argument handling and headless
//! (`--print`) runs. Every run uses a scratch harness home and workdir.

use std::path::Path;
use std::process::{Command, Output};

fn arbeharness(home: &Path, workdir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_arbeharness"));
    command
        .arg("--dev-home")
        .arg(home)
        .arg("--workdir")
        .arg(workdir)
        .args(args)
        // Only what the test sets: no ambient ARBE_* settings.
        .env_remove("ARBE_PROVIDER")
        .env_remove("ARBE_MODEL")
        .env_remove("ARBE_BASE_URL")
        .env_remove("ARBE_PROFILE");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("failed to run arbeharness")
}

fn json_lines(output: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}")))
        .collect()
}

#[test]
fn version_help_and_bad_arguments_never_open_the_ui() {
    let version = Command::new(env!("CARGO_BIN_EXE_arbeharness"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("arbeharness {}", env!("CARGO_PKG_VERSION"))
    );

    let help = Command::new(env!("CARGO_BIN_EXE_arbeharness"))
        .arg("-h")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&help.stdout).contains("--resume <ID>"));

    for args in [&["--verison"][..], &["stray"], &["--resume", "nope"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_arbeharness"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("error:"));
    }
}

#[test]
fn a_headless_run_reports_events_and_a_failed_result_as_json() {
    let home = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    // Nothing listens on port 9: the provider is unreachable, which is
    // reported without retrying.
    let out = arbeharness(
        home.path(),
        workdir.path(),
        &[("ARBE_BASE_URL", "http://127.0.0.1:9")],
        &["--print", "hello", "--output", "json", "--name", "probe"],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = json_lines(&out);
    assert_eq!(lines.first().unwrap()["type"], "turn_started");
    let result = lines.last().unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["exit_code"], 1);
    assert!(result["answer"].is_null() && result["error"].is_string());

    // The session was saved: named, located, and closed.
    let id = result["session_id"].as_str().unwrap();
    let meta: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.path().join("sessions").join(id).join("meta.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(meta["title"], "probe");
    assert_eq!(meta["status"], "closed");
    assert_eq!(
        Path::new(meta["workdir"].as_str().unwrap()),
        std::path::absolute(workdir.path()).unwrap()
    );
    assert!(meta.get("pid").is_none() && meta.get("activity").is_none());
}

/// Provider settings for the live test: an OpenAI-compatible server
/// (`ARBE_LIVE_COMPAT_BASE_URL`, `ARBE_LIVE_COMPAT_MODEL`,
/// `ARBE_LIVE_COMPAT_API_KEY`) or local Ollama (`ARBE_LIVE_OLLAMA=1`).
fn live_env() -> Option<Vec<(&'static str, String)>> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if let Some(url) = var("ARBE_LIVE_COMPAT_BASE_URL") {
        let mut env = vec![
            ("ARBE_PROVIDER", "openai_compatible".to_string()),
            ("ARBE_BASE_URL", url),
            ("ARBE_MODEL", var("ARBE_LIVE_COMPAT_MODEL")?),
        ];
        if let Some(key) = var("ARBE_LIVE_COMPAT_API_KEY") {
            env.push(("ARBE_API_KEY", key));
        }
        return Some(env);
    }
    var("ARBE_LIVE_OLLAMA").map(|_| Vec::new())
}

/// `cargo test --test cli -- --ignored` with a live target (see [`live_env`]).
#[test]
#[ignore = "needs a live model"]
fn a_headless_run_answers_resumes_and_honors_the_approval_policy() {
    let Some(live) = live_env() else {
        eprintln!("skipped: set ARBE_LIVE_COMPAT_BASE_URL or ARBE_LIVE_OLLAMA");
        return;
    };
    let live: Vec<(&str, &str)> = live.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let home = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    let file = workdir.path().join("server.toml");
    std::fs::write(&file, "port = 8742\n").unwrap();

    let out = arbeharness(
        home.path(),
        workdir.path(),
        &live,
        &[
            "--print",
            "Which port does server.toml set? Read it.",
            "--approve",
            "reads",
            "--output",
            "json",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result = json_lines(&out).pop().unwrap();
    assert!(
        result["answer"].as_str().unwrap().contains("8742"),
        "{result}"
    );
    let id = result["session_id"].as_str().unwrap().to_string();

    // `reads` doesn't cover edits...
    let out = arbeharness(
        home.path(),
        workdir.path(),
        &live,
        &[
            "--resume",
            &id,
            "--print",
            "Change the port in server.toml to 9100.",
            "--approve",
            "reads",
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("[denied]"));
    assert!(std::fs::read_to_string(&file).unwrap().contains("8742"));

    // ...`all` does.
    let out = arbeharness(
        home.path(),
        workdir.path(),
        &live,
        &[
            "--resume",
            &id,
            "--print",
            "Change the port in server.toml to 9100.",
            "--approve",
            "all",
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(std::fs::read_to_string(&file).unwrap().contains("9100"));
}

#[test]
fn headless_mode_speaks_json_rpc_over_stdio() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    let home = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_arbeharness"))
        .arg("--headless")
        .arg("--dev-home")
        .arg(home.path())
        .arg("--workdir")
        .arg(workdir.path())
        .env_remove("ARBE_PROVIDER")
        .env_remove("ARBE_MODEL")
        // Unreachable: the turn fails fast, without a real model.
        .env("ARBE_BASE_URL", "http://127.0.0.1:9")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut request = |id: u32, method: &str, params: serde_json::Value| {
        let message =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(stdin, "{message}").unwrap();
    };
    let mut read_until_response = |id: u32| {
        let mut before = Vec::new();
        loop {
            let line = lines.next().expect("stdout closed").unwrap();
            let message: serde_json::Value = serde_json::from_str(&line).unwrap();
            if message["id"] == id {
                return (message, before);
            }
            before.push(message);
        }
    };

    request(1, "initialize", serde_json::Value::Null);
    let (init, _) = read_until_response(1);
    assert_eq!(init["result"]["server"], "arbeharness");

    request(2, "session/new", serde_json::json!({"name": "via rpc"}));
    let (opened, _) = read_until_response(2);
    let session_id = opened["result"]["session_id"].clone();

    request(
        3,
        "turn/send",
        serde_json::json!({"session_id": session_id, "message": "hi"}),
    );
    let (failed, events) = read_until_response(3);
    assert_eq!(failed["error"]["code"], -32003, "{failed}");
    let types: Vec<&str> = events
        .iter()
        .filter(|m| m["method"] == "event")
        .map(|m| m["params"]["event"]["type"].as_str().unwrap())
        .collect();
    assert!(types.contains(&"turn_started"), "{types:?}");
    assert!(types.contains(&"runtime_error"), "{types:?}");

    request(4, "shutdown", serde_json::Value::Null);
    let (bye, _) = read_until_response(4);
    assert_eq!(bye["result"], serde_json::json!({}));
    assert!(child.wait().unwrap().success());
}
