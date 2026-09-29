//! The per-turn hot path: assembling a request's context from session
//! history, including pruning old tool output and trimming to the budget.
//! Runs on every model call, so its cost grows with session length.
//!
//! `cargo bench -p arbe-memory`

use arbe_core::{Message, RequestedToolCall, Role};
use arbe_memory::{ContextPipeline, HistoryEntry, TruncationStrategy};
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use serde_json::json;

/// `turns` coding-style turns: a question, a tool call, an 8 KB tool
/// result, and an answer.
fn history(turns: u64) -> Vec<HistoryEntry> {
    let output = "fn example() { let x = 42; }\n".repeat(280); // ~8 KB
    (0..turns)
        .flat_map(|i| {
            let id = format!("call-{i}");
            [
                Message::new(
                    Role::User,
                    format!("question {i}: what does src/lib.rs do?"),
                ),
                Message::assistant_tool_calls(vec![RequestedToolCall {
                    id: id.clone(),
                    name: "read_file".into(),
                    arguments: json!({"path": "src/lib.rs"}),
                }]),
                Message::tool_result(id, output.clone()),
                Message::new(
                    Role::Assistant,
                    format!("answer {i}: it defines example()."),
                ),
            ]
            .into_iter()
            .map(move |message| HistoryEntry {
                turn_index: i,
                message,
            })
        })
        .collect()
}

fn assemble(c: &mut Criterion) {
    let pipeline = ContextPipeline {
        system_instructions: vec!["You are a coding agent. ".repeat(200)],
        ..Default::default()
    };
    let mut group = c.benchmark_group("context_assembly");
    for turns in [50, 500, 2_000] {
        let history = history(turns);
        // A 128k-token model with room left for the answer.
        let budget = 120_000;
        group.bench_with_input(
            BenchmarkId::from_parameter(turns),
            &history,
            |b, history| {
                b.iter(|| {
                    pipeline.assemble(
                        &TruncationStrategy,
                        black_box(history),
                        &[],
                        Message::new(Role::User, "next question"),
                        budget,
                    )
                })
            },
        );
    }
    group.finish();
}

criterion_group!(benches, assemble);
criterion_main!(benches);
