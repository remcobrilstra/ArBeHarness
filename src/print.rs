//! `--print`: one turn without a UI. The answer goes to stdout (or, with
//! `--output json`, every event and then the result as JSON lines); tool
//! activity and problems go to stderr. Nobody is there to approve tool
//! calls, so `--approve` decides them up front.

use std::collections::HashMap;
use std::io::Write;

use arbe_tui::arbe_runtime::Harness;
use arbe_tui::arbe_runtime::arbe_core::{
    ApprovalDecision, HarnessError, RiskLevel, RuntimeEvent, StopReason, ToolCallId,
};
use serde_json::json;

use crate::cli::{ApprovePolicy, Cli, OutputFormat};

const EXIT_OK: i32 = 0;
const EXIT_FAILED: i32 = 1;
const EXIT_INCOMPLETE: i32 = 3;
const EXIT_INTERRUPTED: i32 = 130;

pub async fn run(harness: &Harness, cli: &Cli, prompt: String) -> i32 {
    let session = match cli.resume {
        Some(id) => harness.resume_session(id),
        None => harness.new_session(),
    };
    let session = match session {
        Ok(session) => session,
        Err(err) => {
            eprintln!("error: failed to open the session: {err}");
            return EXIT_FAILED;
        }
    };
    if let Some(name) = &cli.name
        && let Err(err) = session.set_title(name.clone())
    {
        eprintln!("warning: failed to name the session: {err}");
    }
    let json = cli.output == OutputFormat::Json;

    let mut turn = session.send(prompt);
    let mut stop_reason = None;
    // Each proposed call's risk, for deciding its approval request.
    let mut risks: HashMap<ToolCallId, RiskLevel> = HashMap::new();
    let mut interrupted = false;
    let mut stdout = std::io::stdout().lock();
    loop {
        let event = tokio::select! {
            event = turn.next_event() => event,
            _ = tokio::signal::ctrl_c(), if !interrupted => {
                interrupted = true;
                eprintln!("interrupted; stopping the turn");
                session.cancel();
                continue;
            }
        };
        let Some(event) = event else { break };
        if json {
            // A closed stdout (e.g. piped into `head`) isn't worth failing
            // the turn over.
            if let Ok(line) = serde_json::to_string(&event) {
                let _ = writeln!(stdout, "{line}");
            }
        }
        // A subagent's events arrive wrapped; unwrap them, and indent
        // their lines by how deeply they're nested.
        let (inner, depth) = event.innermost();
        let indent = "  ".repeat(depth);
        match inner {
            RuntimeEvent::ToolCallProposed {
                tool_call_id,
                tool_name,
                arguments,
                risk,
                ..
            } => {
                if !json {
                    eprintln!("{indent}[tool] {tool_name} {arguments}");
                }
                risks.insert(*tool_call_id, *risk);
            }
            RuntimeEvent::ToolApprovalRequested { tool_call_id, .. } => {
                let approve = match cli.approve {
                    ApprovePolicy::All => true,
                    ApprovePolicy::Reads => risks.get(tool_call_id) == Some(&RiskLevel::Low),
                    ApprovePolicy::None => false,
                };
                let decision = if approve {
                    ApprovalDecision::ApprovedOnce
                } else {
                    ApprovalDecision::DeniedOnce
                };
                session.decide(*tool_call_id, decision);
            }
            RuntimeEvent::ToolCallDenied {
                tool_name, reason, ..
            } if !json => eprintln!("{indent}[denied] {tool_name}: {reason}"),
            RuntimeEvent::ProviderRetrying {
                attempt, reason, ..
            } if !json => eprintln!("{indent}[retry {attempt}] {reason}"),
            // Only the top-level turn's ending decides the exit status.
            RuntimeEvent::TurnCompleted {
                stop_reason: reason,
                ..
            } if depth == 0 => stop_reason = Some(reason.clone()),
            _ => {}
        }
    }
    let result = turn.finish().await;

    let code = match (&result, &stop_reason) {
        (Ok(_), Some(StopReason::EndTurn)) => EXIT_OK,
        (Ok(_), _) => EXIT_INCOMPLETE,
        (Err(HarnessError::Cancelled), _) => EXIT_INTERRUPTED,
        (Err(_), _) => EXIT_FAILED,
    };
    if json {
        let line = json!({
            "type": "result",
            "session_id": session.id().to_string(),
            "exit_code": code,
            "stop_reason": stop_reason,
            "answer": result.as_ref().ok(),
            "error": result.as_ref().err().map(|e| e.to_string()),
        });
        let _ = writeln!(stdout, "{line}");
    } else {
        match &result {
            Ok(answer) => {
                let _ = writeln!(stdout, "{answer}");
                if code == EXIT_INCOMPLETE {
                    eprintln!("stopped before finishing: {stop_reason:?}");
                }
            }
            Err(err) => eprintln!("error: {err}"),
        }
    }
    let _ = stdout.flush();
    if let Err(err) = session.close() {
        eprintln!("warning: failed to close the session: {err}");
    }
    code
}
