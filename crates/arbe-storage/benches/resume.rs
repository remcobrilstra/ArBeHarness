//! Resuming a session reads its whole `turns.jsonl` (the history is
//! rebuilt from every turn). How long that takes for large sessions.
//!
//! `cargo bench -p arbe-storage`

use arbe_core::{Message, Role, Turn};
use arbe_storage::SessionStore;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

fn resume(c: &mut Criterion) {
    let mut group = c.benchmark_group("list_turns");
    group.sample_size(10);
    for turns in [100u64, 1_000, 5_000] {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::with_root(dir.path().to_path_buf());
        let meta = store.create_session("coding", "bench", "bench").unwrap();
        let output = "fn example() { let x = 42; }\n".repeat(280); // ~8 KB
        for i in 0..turns {
            let mut turn = Turn::new(meta.id, i);
            turn.messages = vec![
                Message::new(Role::User, format!("question {i}")),
                Message::tool_result(format!("call-{i}"), output.clone()),
                Message::new(Role::Assistant, format!("answer {i}")),
            ];
            store.append_turn(&turn).unwrap();
        }
        let bytes = std::fs::metadata(dir.path().join(meta.id.to_string()).join("turns.jsonl"))
            .map(|m| m.len())
            .unwrap_or(0);
        eprintln!("{turns} turns: turns.jsonl is {:.1} MB", bytes as f64 / 1e6);
        group.bench_with_input(BenchmarkId::from_parameter(turns), &meta.id, |b, id| {
            b.iter(|| store.list_turns(*id).unwrap())
        });
    }
    group.finish();
}

criterion_group!(benches, resume);
criterion_main!(benches);
