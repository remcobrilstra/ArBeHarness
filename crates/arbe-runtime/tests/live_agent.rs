//! End-to-end runs of the whole harness against a real model (v2 plan
//! P7.2): the real system prompt, builtin tools, approvals, persistence,
//! memory, skills, compaction, cancellation and resume — each on a small
//! task in a scratch project, with a scratch harness home.
//!
//! `#[ignore]`d. Pick a target:
//!
//! ```text
//! # any OpenAI-compatible server, e.g. xAI:
//! ARBE_LIVE_COMPAT_BASE_URL=https://api.x.ai/v1 ARBE_LIVE_COMPAT_API_KEY=... \
//! ARBE_LIVE_COMPAT_MODEL=grok-4.7 \
//!   cargo test -p arbe-runtime --test live_agent -- --ignored --nocapture
//!
//! # local Ollama with the default models (qwen2.5-coder:3b, llama3.2:3b):
//! ARBE_LIVE_OLLAMA=1 cargo test -p arbe-runtime --test live_agent -- --ignored --nocapture
//! ```
//!
//! Every test prints the tool calls it saw, so a failure shows what the
//! model actually did. Model behavior varies; the assertions check
//! outcomes (files changed, facts recalled), not exact wording.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arbe_core::{ApprovalDecision, HarnessError, RuntimeEvent, StopReason};
use arbe_runtime::{Agent, EventBus, RuntimeConfig};
use arbe_storage::SessionStore;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Which model service the tests run against.
struct Target {
    settings: Vec<(&'static str, String)>,
    /// Whether the model can look at images (the default Ollama models
    /// can't).
    vision: bool,
    coding_model: String,
    general_model: String,
}

fn target() -> Option<Target> {
    if let Some(base_url) = env("ARBE_LIVE_COMPAT_BASE_URL") {
        let model = env("ARBE_LIVE_COMPAT_MODEL").expect("set ARBE_LIVE_COMPAT_MODEL");
        let mut settings = vec![
            ("ARBE_PROVIDER", "openai_compatible".to_string()),
            ("ARBE_BASE_URL", base_url),
        ];
        if let Some(key) = env("ARBE_LIVE_COMPAT_API_KEY") {
            settings.push(("ARBE_API_KEY", key));
        }
        return Some(Target {
            settings,
            vision: true,
            coding_model: model.clone(),
            general_model: model,
        });
    }
    env("ARBE_LIVE_OLLAMA")?;
    let mut settings = vec![("ARBE_PROVIDER", "ollama".to_string())];
    if let Some(url) = env("ARBE_BASE_URL") {
        settings.push(("ARBE_BASE_URL", url));
    }
    Some(Target {
        settings,
        vision: false,
        coding_model: env("ARBE_LIVE_CODING_MODEL").unwrap_or_else(|| "qwen2.5-coder:3b".into()),
        general_model: env("ARBE_LIVE_GENERAL_MODEL").unwrap_or_else(|| "llama3.2:3b".into()),
    })
}

/// Skips the test (returning early) when no target is configured.
macro_rules! target_or_skip {
    () => {
        match target() {
            Some(target) => target,
            None => {
                eprintln!("skipped: set ARBE_LIVE_COMPAT_BASE_URL or ARBE_LIVE_OLLAMA");
                return;
            }
        }
    };
}

/// A scratch harness home + project, and how to open agents on them.
struct Bench {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
    target: Target,
    profile: &'static str,
    /// Extra config (TOML) applied as a global config file.
    config_toml: String,
}

impl Bench {
    fn new(target: Target, profile: &'static str) -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
            target,
            profile,
            config_toml: String::new(),
        }
    }

    fn project(&self) -> &Path {
        self.project.path()
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.project().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.project().join(relative)).unwrap_or_default()
    }

    fn config(&self) -> RuntimeConfig {
        let model = if self.profile == "general" {
            &self.target.general_model
        } else {
            &self.target.coding_model
        };
        let mut vars: HashMap<&str, String> = self.target.settings.iter().cloned().collect();
        vars.insert("ARBE_PROFILE", self.profile.to_string());
        vars.insert("ARBE_MODEL", model.clone());
        vars.insert("ARBE_HOME", self.home.path().display().to_string());
        let lookup = move |key: &str| vars.get(key).cloned();
        let file = self.home.path().join("live.toml");
        std::fs::write(&file, &self.config_toml).unwrap();
        RuntimeConfig::load_from(&[file], &lookup, self.project().to_path_buf()).unwrap()
    }

    fn store(&self) -> SessionStore {
        SessionStore::with_root(self.home.path().join("sessions"))
    }

    fn start(&self, approve: Approve) -> Run {
        let events = Arc::new(EventBus::new(8192));
        let rx = events.subscribe();
        let agent = Arc::new(Agent::create(&self.config(), self.store(), events).unwrap());
        Run::watch(agent, rx, approve)
    }

    fn resume(&self, id: arbe_core::SessionId, approve: Approve) -> Run {
        let events = Arc::new(EventBus::new(8192));
        let rx = events.subscribe();
        let agent = Arc::new(Agent::resume(&self.config(), self.store(), id, events).unwrap());
        Run::watch(agent, rx, approve)
    }
}

#[derive(Clone, Copy)]
enum Approve {
    All,
    /// Approve everything except these tools.
    AllBut(&'static [&'static str]),
}

/// What one turn did.
#[derive(Debug, Default)]
struct Trace {
    /// `(tool, arguments)` for every proposed call.
    calls: Vec<(String, String)>,
    denied: Vec<String>,
    thinking_chars: usize,
    /// Questions the model asked (`ask_user`), each answered with its first
    /// option (or "yes").
    questions: Vec<String>,
    stop: Option<StopReason>,
    compactions: usize,
    /// `(tool, output)` for every executed call.
    results: Vec<(String, String)>,
}

impl Trace {
    fn used(&self, tool: &str) -> bool {
        self.calls.iter().any(|(t, _)| t == tool)
    }
}

/// An MCP server that connected `(name, tool count)` or failed `(name, reason)`.
type McpStatus = Result<(String, usize), (String, String)>;

struct Run {
    agent: Arc<Agent>,
    trace: Arc<Mutex<Trace>>,
    /// MCP servers that connected (name, tool count) or failed (name, reason).
    mcp: Arc<Mutex<Vec<McpStatus>>>,
}

impl Run {
    fn watch(
        agent: Arc<Agent>,
        mut rx: tokio::sync::broadcast::Receiver<arbe_core::EventEnvelope>,
        approve: Approve,
    ) -> Self {
        let trace = Arc::new(Mutex::new(Trace::default()));
        let mcp = Arc::new(Mutex::new(Vec::new()));
        let (a, t, m) = (agent.clone(), trace.clone(), mcp.clone());
        tokio::spawn(async move {
            let mut names = HashMap::new();
            while let Ok(envelope) = rx.recv().await {
                // A subagent's events arrive wrapped; its calls are recorded
                // as `↳tool`, and only the top-level turn's ending counts.
                let (event, depth) = envelope.event.innermost();
                let (event, prefix) = (event.clone(), "↳".repeat(depth));
                match event {
                    RuntimeEvent::ToolCallProposed {
                        tool_call_id,
                        tool_name,
                        arguments,
                        ..
                    } => {
                        eprintln!("    tool: {prefix}{tool_name} {arguments}");
                        names.insert(tool_call_id, tool_name.clone());
                        let tool_name = format!("{prefix}{tool_name}");
                        t.lock()
                            .unwrap()
                            .calls
                            .push((tool_name, arguments.to_string()));
                    }
                    RuntimeEvent::ToolApprovalRequested { tool_call_id, .. } => {
                        let name = names.get(&tool_call_id).cloned().unwrap_or_default();
                        let approved = match approve {
                            Approve::All => true,
                            Approve::AllBut(denied) => !denied.contains(&name.as_str()),
                        };
                        a.supply_tool_decision(
                            tool_call_id,
                            if approved {
                                ApprovalDecision::ApprovedOnce
                            } else {
                                ApprovalDecision::DeniedOnce
                            },
                        );
                    }
                    RuntimeEvent::UserQuestionAsked {
                        question_id,
                        question,
                        options,
                        ..
                    } => {
                        let reply = options.first().cloned().unwrap_or_else(|| "yes".into());
                        eprintln!("    {prefix}question: {question} -> {reply}");
                        t.lock().unwrap().questions.push(question);
                        a.answer_question(question_id, reply);
                    }
                    RuntimeEvent::ToolCallDenied { tool_name, .. } => {
                        t.lock().unwrap().denied.push(tool_name);
                    }
                    RuntimeEvent::ThinkingDelta { delta, .. } => {
                        t.lock().unwrap().thinking_chars += delta.len();
                    }
                    RuntimeEvent::ToolExecuted {
                        tool_name, result, ..
                    } => {
                        t.lock()
                            .unwrap()
                            .results
                            .push((tool_name, result.output.to_string()));
                    }
                    RuntimeEvent::McpServerConnected { server, tools } => {
                        m.lock().unwrap().push(Ok((server, tools)));
                    }
                    RuntimeEvent::McpServerFailed { server, reason } => {
                        m.lock().unwrap().push(Err((server, reason)));
                    }
                    RuntimeEvent::CompactionPerformed { .. } => {
                        t.lock().unwrap().compactions += 1;
                    }
                    RuntimeEvent::TurnCompleted { stop_reason, .. } if depth == 0 => {
                        t.lock().unwrap().stop = Some(stop_reason);
                    }
                    _ => {}
                }
            }
        });
        Self { agent, trace, mcp }
    }

    /// Waits for the first MCP server to connect; panics if it fails.
    async fn wait_for_mcp(&self) -> usize {
        for _ in 0..600 {
            if let Some(status) = self.mcp.lock().unwrap().first() {
                match status {
                    Ok((_, tools)) => return *tools,
                    Err((server, reason)) => panic!("MCP server {server} failed: {reason}"),
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        panic!("MCP server didn't connect within 2 minutes");
    }

    async fn ask(&self, prompt: &str) -> (String, Trace) {
        eprintln!("  > {prompt}");
        let answer = tokio::time::timeout(
            Duration::from_secs(300),
            self.agent.submit_message(prompt.to_string()),
        )
        .await
        .expect("turn took over 5 minutes")
        .unwrap_or_else(|e| panic!("turn failed: {e}"));
        let trace = self.take_trace().await;
        eprintln!(
            "  < {answer:?} ({:?}, {} chars of thinking)",
            trace.stop, trace.thinking_chars
        );
        (answer, trace)
    }

    async fn take_trace(&self) -> Trace {
        // Let the watcher drain the turn's last events.
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::mem::take(&mut *self.trace.lock().unwrap())
    }
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

#[tokio::test]
#[ignore = "needs a live model"]
async fn reads_and_edits_files() {
    let bench = Bench::new(target_or_skip!(), "coding");
    bench.write("server.toml", "[server]\nhost = \"0.0.0.0\"\nport = 8742\n");
    let run = bench.start(Approve::All);

    let (answer, trace) = run
        .ask("Which port does server.toml configure? Look at the file.")
        .await;
    assert!(trace.used("read_file") || trace.used("grep"), "{trace:?}");
    assert!(answer.contains("8742"), "{answer}");
    assert_eq!(trace.stop, Some(StopReason::EndTurn));

    run.ask("Change the port in server.toml to 9100. Keep everything else as is.")
        .await;
    let file = bench.read("server.toml");
    assert!(
        file.contains("9100") && file.contains("host = \"0.0.0.0\""),
        "{file}"
    );
}

#[tokio::test]
#[ignore = "needs a live model"]
async fn a_denied_edit_is_reported_and_leaves_the_file_alone() {
    let bench = Bench::new(target_or_skip!(), "coding");
    bench.write("notes.txt", "keep me\n");
    let run = bench.start(Approve::AllBut(&["write_file", "edit_file", "execute"]));

    let (answer, trace) = run
        .ask("Replace the contents of notes.txt with the word 'gone'.")
        .await;
    assert!(
        !trace.denied.is_empty(),
        "no write was attempted: {trace:?}"
    );
    assert_eq!(bench.read("notes.txt"), "keep me\n");
    assert_eq!(trace.stop, Some(StopReason::EndTurn));
    eprintln!("  (answer after denial: {answer:?})");
}

#[tokio::test]
#[ignore = "needs a live model"]
async fn fixes_a_bug_by_running_the_tests_in_a_loop() {
    let bench = Bench::new(target_or_skip!(), "coding");
    let Some(python) = python() else {
        eprintln!("skipped: no python on PATH");
        return;
    };
    bench.write(
        "calc.py",
        "def average(values):\n    return sum(values) / len(values) + 1\n",
    );
    bench.write(
        "test_calc.py",
        "from calc import average\n\nassert average([2, 4]) == 3, average([2, 4])\nassert average([5]) == 5\nprint('all tests passed')\n",
    );
    let run = bench.start(Approve::All);

    let (_, trace) = run
        .ask(&format!(
            "The tests in test_calc.py fail. Run them with `{python} test_calc.py`, fix the bug in calc.py (not the tests), and run them again to confirm."
        ))
        .await;
    assert!(trace.used("execute"), "never ran the tests: {trace:?}");
    assert!(
        trace.used("edit_file") || trace.used("write_file"),
        "{trace:?}"
    );
    let out = std::process::Command::new(python)
        .arg("test_calc.py")
        .current_dir(bench.project())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "tests still fail: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(bench.read("test_calc.py").contains("average([2, 4]) == 3"));
}

#[tokio::test]
#[ignore = "needs a live model"]
async fn a_resumed_session_remembers_the_conversation() {
    let bench = Bench::new(target_or_skip!(), "general");
    let run = bench.start(Approve::All);
    run.ask("My project's codename is BLUE-HERON-42. Just acknowledge it.")
        .await;
    let id = run.agent.session_id();
    run.agent.close().unwrap();
    drop(run);

    let resumed = bench.resume(id, Approve::All);
    let (answer, _) = resumed.ask("What is my project's codename?").await;
    assert!(answer.contains("BLUE-HERON-42"), "{answer}");
}

#[tokio::test]
#[ignore = "needs a live model"]
async fn remembered_notes_carry_over_to_a_new_session() {
    let bench = Bench::new(target_or_skip!(), "general");
    let run = bench.start(Approve::All);
    let (_, trace) = run
        .ask("Please remember for future sessions: I always want answers in British English, and my name is Remco.")
        .await;
    assert!(trace.used("remember"), "{trace:?}");
    drop(run);

    // A brand new session (no shared history) sees the note.
    let fresh = bench.start(Approve::All);
    let (answer, _) = fresh.ask("What's my name?").await;
    assert!(answer.contains("Remco"), "{answer}");
}

#[tokio::test]
#[ignore = "needs a live model"]
async fn loads_a_skill_on_demand_and_follows_it() {
    let bench = Bench::new(target_or_skip!(), "coding");
    std::fs::create_dir_all(bench.home.path().join("skills")).unwrap();
    std::fs::write(
        bench.home.path().join("skills").join("release-notes.md"),
        "---\nname: release-notes\ndescription: How to write release notes for this team. Use whenever asked to write release notes.\n---\nRelease notes must start with the exact line `== RELEASE NOTES ==` and end with the exact line `-- signed, the build bot`.\n",
    )
    .unwrap();
    let run = bench.start(Approve::All);
    let (answer, trace) = run
        .ask("Write release notes for version 1.2: we fixed a crash on startup. Reply with the notes only.")
        .await;
    assert!(trace.used("load_skill"), "{trace:?}");
    assert!(
        answer.contains("== RELEASE NOTES ==") && answer.contains("-- signed, the build bot"),
        "{answer}"
    );
}

#[tokio::test]
#[ignore = "needs a live model"]
async fn nested_instructions_apply_once_their_folder_is_touched() {
    let bench = Bench::new(target_or_skip!(), "coding");
    bench.write(
        "billing/AGENTS.md",
        "In the billing folder, amounts are stored in cents. Whenever you report an amount from this folder, also state it in euros.",
    );
    bench.write("billing/prices.toml", "basic_plan = 1999\n");
    let run = bench.start(Approve::All);
    let (answer, trace) = run
        .ask("What is the basic plan price in billing/prices.toml?")
        .await;
    assert!(trace.used("read_file") || trace.used("grep"), "{trace:?}");
    assert!(answer.contains("19.99"), "{answer}");
}

#[tokio::test]
#[ignore = "needs a live model"]
async fn compaction_keeps_what_matters() {
    let mut bench = Bench::new(target_or_skip!(), "general");
    bench.config_toml = "[context]\nmemory_strategy = \"compact_summary\"\n".into();
    let run = bench.start(Approve::All);
    run.ask("Note this for later in our conversation: the deploy window is Thursday 14:00 UTC. Just acknowledge.")
        .await;
    run.ask("Unrelated: name three primary colours, briefly.")
        .await;
    assert!(run.agent.compact().await.unwrap(), "nothing was compacted");
    assert_eq!(run.take_trace().await.compactions, 1);

    let (answer, _) = run.ask("When is the deploy window?").await;
    assert!(
        answer.contains("14:00") && contains_ci(&answer, "thursday"),
        "{answer}"
    );
}

#[tokio::test]
#[ignore = "needs a live model"]
async fn a_cancelled_turn_is_saved_and_the_session_continues() {
    let bench = Bench::new(target_or_skip!(), "general");
    let run = bench.start(Approve::All);
    let agent = run.agent.clone();
    let turn = tokio::spawn(async move {
        agent
            .submit_message("Write a 2000-word essay about the history of bridges.".into())
            .await
    });
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(run.agent.cancel_turn());
    let result = turn.await.unwrap();
    assert!(matches!(result, Err(HarnessError::Cancelled)), "{result:?}");

    let turns = bench.store().list_turns(run.agent.session_id()).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].stop_reason, Some(StopReason::Cancelled));

    let (answer, _) = run.ask("Reply with just the word: ready").await;
    assert!(contains_ci(&answer, "ready"), "{answer}");
}

#[tokio::test]
#[ignore = "needs a live model"]
async fn the_general_profile_answers_without_file_tools() {
    let bench = Bench::new(target_or_skip!(), "general");
    bench.write("secret.txt", "do not read\n");
    let run = bench.start(Approve::All);
    let (answer, trace) = run.ask("What is the capital of France? One word.").await;
    assert!(contains_ci(&answer, "paris"), "{answer}");
    assert!(!trace.used("read_file"), "{trace:?}");
    if !trace.calls.is_empty() {
        eprintln!(
            "  note: needless tool use for a plain question: {:?}",
            trace.calls
        );
    }
}

fn python() -> Option<&'static str> {
    ["python", "python3"].into_iter().find(|p| {
        std::process::Command::new(p)
            .arg("--version")
            .output()
            .is_ok()
    })
}

#[tokio::test]
#[ignore = "needs a live model, Node/npx and network"]
async fn uses_tools_from_an_mcp_server() {
    let mut bench = Bench::new(target_or_skip!(), "coding");
    bench.config_toml = r#"
[mcp.servers.everything]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-everything"]
timeout_secs = 120
"#
    .into();
    let run = bench.start(Approve::All);
    assert!(run.wait_for_mcp().await > 0);

    let (answer, trace) = run
        .ask(
            "Use a tool from the `everything` MCP server to add 17 and 25, and tell me the result.",
        )
        .await;
    // Tool names vary between versions of the reference server (`add`,
    // `get-sum`, ...); what matters is that an MCP tool did the sum.
    assert!(
        trace
            .results
            .iter()
            .any(|(tool, out)| tool.starts_with("everything__") && out.contains("42")),
        "{trace:?}"
    );
    assert!(answer.contains("42"), "{answer}");
}

#[tokio::test]
#[ignore = "needs a live model and python"]
async fn a_hook_can_veto_a_models_command() {
    let mut bench = Bench::new(target_or_skip!(), "coding");
    let Some(python) = python() else {
        eprintln!("skipped: no python on PATH");
        return;
    };
    let guard = bench.home.path().join("guard.py");
    std::fs::write(
        &guard,
        r#"import json, sys
call = json.load(sys.stdin)
if call["tool_name"] == "execute":
    print(json.dumps({"veto": "shell commands are disabled by policy"}))
"#,
    )
    .unwrap();
    // Forward slashes: valid on every OS, and no TOML escaping needed.
    let guard = guard.display().to_string().replace('\\', "/");
    bench.config_toml = format!(
        "[[hooks.commands]]\nphase = \"before_tool_execute\"\ncommand = '{python} \"{guard}\"'\n"
    );
    let run = bench.start(Approve::All);
    let (answer, trace) = run
        .ask("Run the shell command `echo hook-probe > probe.txt` using the execute tool.")
        .await;
    // A vetoed call is denied before it's ever proposed for approval.
    assert!(trace.denied.iter().any(|t| t == "execute"), "{trace:?}");
    assert!(
        !trace.results.iter().any(|(t, _)| t == "execute"),
        "{trace:?}"
    );
    assert!(
        !bench.project().join("probe.txt").exists(),
        "the command ran"
    );
    eprintln!("  (answer after veto: {answer:?})");
}

#[tokio::test]
#[ignore = "needs a live model"]
async fn delegates_research_to_a_subagent() {
    let bench = Bench::new(target_or_skip!(), "coding");
    bench.write("src/app/main.py", "from app.billing import compute_tax\n");
    bench.write(
        "src/app/billing.py",
        "RATE = 0.21\n\ndef compute_tax(amount):\n    return amount * RATE\n",
    );
    bench.write("src/app/users.py", "def load_users():\n    return []\n");
    let run = bench.start(Approve::All);
    let (answer, trace) = run
        .ask("Use the task tool to have a subagent find out which file defines `compute_tax` and what tax rate it uses. Then tell me both.")
        .await;
    assert!(trace.used("task"), "{trace:?}");
    // The subagent did the looking, not the parent.
    assert!(
        trace.calls.iter().any(|(t, _)| t.starts_with('↳')),
        "{trace:?}"
    );
    assert!(
        answer.contains("billing.py") && (answer.contains("0.21") || answer.contains("21%")),
        "{answer}"
    );
}

#[tokio::test]
#[ignore = "needs a live model that can see images"]
async fn reads_an_image_file_and_sees_it() {
    let bench = Bench::new(target_or_skip!(), "coding");
    if !bench.target.vision {
        eprintln!("skipped: this target's model has no vision");
        return;
    }
    // A 32x32 solid crimson PNG.
    const RED_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAKklEQVR4nGO4I2JDU8QwasGoBaMWjFowasGoBaMWjFowasGoBaMWDBULADahsD1ndvqVAAAAAElFTkSuQmCC";
    use base64::Engine;
    let png = base64::engine::general_purpose::STANDARD
        .decode(RED_PNG)
        .unwrap();
    std::fs::write(bench.project().join("logo.png"), png).unwrap();
    let run = bench.start(Approve::All);
    let (answer, trace) = run
        .ask("What is the main colour of logo.png? Open it with read_file and look. Answer with one colour word.")
        .await;
    assert!(trace.used("read_file"), "{trace:?}");
    assert!(
        contains_ci(&answer, "red") || contains_ci(&answer, "crimson"),
        "{answer}"
    );
}

#[tokio::test]
#[ignore = "needs a live model"]
async fn asks_the_user_and_follows_the_answer() {
    let bench = Bench::new(target_or_skip!(), "coding");
    let run = bench.start(Approve::All);
    let (_, trace) = run
        .ask("I want a hello-world program in this folder, but I haven't decided on the language. Ask me which one (offer exactly two options: Python first, then Go), then write it as hello.py or hello.go accordingly.")
        .await;
    assert_eq!(trace.questions.len(), 1, "{trace:?}");
    // The watcher answers with the first option.
    assert!(bench.project().join("hello.py").exists(), "{trace:?}");
    assert!(!bench.project().join("hello.go").exists());
}

#[tokio::test]
#[ignore = "needs a live model and python"]
async fn runs_a_server_in_the_background_and_stops_it() {
    let bench = Bench::new(target_or_skip!(), "coding");
    let Some(python) = python() else {
        eprintln!("skipped: no python on PATH");
        return;
    };
    bench.write("index.html", "<h1>hello from the test</h1>");
    let run = bench.start(Approve::All);
    let (_, trace) = run
        .ask(&format!(
            "Start `{python} -m http.server 8765` in the background, use process_output (with a few seconds of wait) to confirm it's serving, then stop it with process_kill. Tell me what the server printed."
        ))
        .await;
    assert!(
        trace
            .calls
            .iter()
            .any(|(t, args)| t == "execute" && args.contains("\"background\":true")),
        "{trace:?}"
    );
    assert!(trace.used("process_output"), "{trace:?}");
    assert!(trace.used("process_kill"), "{trace:?}");
    // The port is free again.
    assert!(std::net::TcpListener::bind("127.0.0.1:8765").is_ok());
}
