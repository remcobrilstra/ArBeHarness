//! A minimal embedder: one question to the agent, answer streamed to
//! stdout, low-risk tool calls approved and everything else denied.
//!
//! ```text
//! cargo run -p arbe-runtime --example embed -- "What does this project do?"
//! ```
//!
//! Uses the same configuration as the `arbeharness` binary (config files,
//! `ARBE_*` variables), in the current directory.

use arbe_runtime::Harness;
use arbe_runtime::arbe_core::{ApprovalDecision, RiskLevel, RuntimeEvent};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let question = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "What does this project do?".to_string());

    let harness = Harness::builder().project_dir(".").build()?;
    let session = harness.new_session()?;
    let mut turn = session.send(question);
    let mut risks = std::collections::HashMap::new();
    while let Some(event) = turn.next_event().await {
        match event {
            RuntimeEvent::ModelStreamChunk { delta, .. } => print!("{delta}"),
            RuntimeEvent::ToolCallProposed {
                tool_call_id,
                tool_name,
                risk,
                ..
            } => {
                eprintln!("\n[tool] {tool_name}");
                risks.insert(tool_call_id, risk);
            }
            RuntimeEvent::ToolApprovalRequested { tool_call_id, .. } => {
                let decision = if risks.get(&tool_call_id) == Some(&RiskLevel::Low) {
                    ApprovalDecision::ApprovedOnce
                } else {
                    ApprovalDecision::DeniedOnce
                };
                session.decide(tool_call_id, decision);
            }
            _ => {}
        }
    }
    let answer = turn.finish().await;
    println!();
    session.close()?;
    answer?;
    Ok(())
}
