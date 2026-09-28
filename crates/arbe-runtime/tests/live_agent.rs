//! End-to-end runs of the whole harness against a real local model (v2
//! plan P7.2): the real system prompt, builtin tools, approval flow and
//! persistence, on small tasks in a scratch project.
//!
//! `#[ignore]`d; needs a running Ollama with the default models pulled:
//!
//! ```text
//! ARBE_LIVE_OLLAMA=1 cargo test -p arbe-runtime --test live_agent -- --ignored --nocapture
//! ```
//!
//! Models: `ARBE_LIVE_CODING_MODEL` (default `qwen2.5-coder:3b`) and
//! `ARBE_LIVE_GENERAL_MODEL` (default `llama3.2:3b`). Every approval request
//! is approved once, and each run prints the tool calls it saw, so a failure
//! shows what the model actually did.

use std::path::Path;
use std::sync::{Arc, Mutex};

use arbe_core::{ApprovalDecision, RuntimeEvent, StopReason};
use arbe_runtime::{Agent, EventBus, RuntimeConfig};
use arbe_storage::SessionStore;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// What one turn did.
#[derive(Debug, Default)]
struct Trace {
    tools: Vec<String>,
    stop: Option<StopReason>,
}

struct Run {
    agent: Arc<Agent>,
    trace: Arc<Mutex<Trace>>,
}

fn start(profile: &str, model: &str, project: &Path, sessions: &Path) -> Run {
    let vars = [("ARBE_PROFILE", profile), ("ARBE_MODEL", model)];
    let lookup = |key: &str| {
        vars.iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
            .or_else(|| (key == "ARBE_BASE_URL").then(|| env(key)).flatten())
    };
    let config = RuntimeConfig::load_from(&[], &lookup, project.to_path_buf()).unwrap();
    let events = Arc::new(EventBus::new(4096));
    let mut rx = events.subscribe();
    let agent = Arc::new(
        Agent::create(
            &config,
            SessionStore::with_root(sessions.to_path_buf()),
            events,
        )
        .unwrap(),
    );

    let trace = Arc::new(Mutex::new(Trace::default()));
    let (a, t) = (agent.clone(), trace.clone());
    tokio::spawn(async move {
        while let Ok(envelope) = rx.recv().await {
            match envelope.event {
                RuntimeEvent::ToolCallProposed {
                    tool_name,
                    arguments,
                    ..
                } => {
                    eprintln!("    tool: {tool_name} {arguments}");
                    t.lock().unwrap().tools.push(tool_name);
                }
                RuntimeEvent::ToolApprovalRequested { tool_call_id, .. } => {
                    a.supply_tool_decision(tool_call_id, ApprovalDecision::ApprovedOnce);
                }
                RuntimeEvent::TurnCompleted { stop_reason, .. } => {
                    t.lock().unwrap().stop = Some(stop_reason);
                }
                _ => {}
            }
        }
    });
    Run { agent, trace }
}

impl Run {
    async fn ask(&self, prompt: &str) -> (String, Trace) {
        eprintln!("  > {prompt}");
        let answer = self
            .agent
            .submit_message(prompt.to_string())
            .await
            .unwrap_or_else(|e| panic!("turn failed: {e}"));
        // Let the event task drain the turn's last events.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let trace = std::mem::take(&mut *self.trace.lock().unwrap());
        eprintln!("  < {answer:?} ({:?})", trace.stop);
        (answer, trace)
    }
}

#[tokio::test]
#[ignore = "needs a local Ollama server with the default models"]
async fn ollama_end_to_end() {
    if env("ARBE_LIVE_OLLAMA").is_none() {
        eprintln!("skipped (set ARBE_LIVE_OLLAMA=1)");
        return;
    }
    // Keep the user's real ~/.arbe (memory, instructions, skills) out of
    // it. This binary has a single test, so nothing races on the variable.
    let home = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread in this process reads the
    // environment (the only test, before any agent exists).
    unsafe { std::env::set_var("ARBE_HOME", home.path()) };
    let sessions = home.path().join("sessions");

    let coding = env("ARBE_LIVE_CODING_MODEL").unwrap_or_else(|| "qwen2.5-coder:3b".into());
    let general = env("ARBE_LIVE_GENERAL_MODEL").unwrap_or_else(|| "llama3.2:3b".into());
    let mut failures = Vec::new();

    // Coding profile: read a fact from the project, then change a file.
    eprintln!("coding profile, {coding}:");
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("server.toml"),
        "[server]\nhost = \"0.0.0.0\"\nport = 8742\n",
    )
    .unwrap();
    let run = start("coding", &coding, project.path(), &sessions);

    let (answer, trace) = run
        .ask("Which port does server.toml configure? Look at the file.")
        .await;
    if !trace.tools.iter().any(|t| t == "read_file" || t == "grep") {
        failures.push(format!(
            "coding/read: never read the file ({:?})",
            trace.tools
        ));
    }
    if !answer.contains("8742") {
        failures.push(format!("coding/read: answer lacks 8742: {answer:?}"));
    }

    let (_, trace) = run
        .ask("Change the port in server.toml to 9100. Keep everything else as is.")
        .await;
    let file = std::fs::read_to_string(project.path().join("server.toml")).unwrap();
    if !file.contains("9100") || !file.contains("host = \"0.0.0.0\"") {
        failures.push(format!(
            "coding/edit: file not changed as asked ({:?}): {file:?}",
            trace.tools
        ));
    }

    // General profile: a plain question, no file tools offered.
    eprintln!("general profile, {general}:");
    let other = tempfile::tempdir().unwrap();
    let run = start("general", &general, other.path(), &sessions);
    let (answer, trace) = run.ask("What is the capital of France? One word.").await;
    if !answer.to_lowercase().contains("paris") {
        failures.push(format!("general/answer: {answer:?}"));
    }
    if trace.stop != Some(StopReason::EndTurn) {
        failures.push(format!("general/answer: stopped with {:?}", trace.stop));
    }
    if !trace.tools.is_empty() {
        // Reported, not failed: small models call whatever tool is offered
        // (llama3.2:3b saves trivia with `remember`); the approval gate is
        // what stops that in real use.
        eprintln!(
            "  note: needless tool use for a plain question: {:?}",
            trace.tools
        );
    }

    // The session was persisted and can be listed.
    let listed = SessionStore::with_root(sessions).list_sessions().unwrap();
    if listed.len() != 2 {
        failures.push(format!(
            "expected 2 persisted sessions, found {}",
            listed.len()
        ));
    }

    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}
