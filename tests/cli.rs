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

/// Needs a local Ollama with the default coding model:
/// `ARBE_LIVE_OLLAMA=1 cargo test --test cli -- --ignored`.
#[test]
#[ignore = "needs a local Ollama server"]
fn a_headless_run_answers_resumes_and_honors_the_approval_policy() {
    if std::env::var("ARBE_LIVE_OLLAMA").is_err() {
        eprintln!("skipped (set ARBE_LIVE_OLLAMA=1)");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    let file = workdir.path().join("server.toml");
    std::fs::write(&file, "port = 8742\n").unwrap();

    let out = arbeharness(
        home.path(),
        workdir.path(),
        &[],
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
        &[],
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
        &[],
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
