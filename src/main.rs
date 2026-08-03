#[tokio::main]
async fn main() {
    // Foundation-only wiring: proves the bin -> arbe-tui -> arbe-runtime
    // dependency chain builds end to end. Real orchestration lands in
    // later phases (see docs/v1-implementation-plan.md).
    let bus = arbe_tui::arbe_runtime::EventBus::default();
    let _subscriber = bus.subscribe();
    println!("ArBeHarness — workspace skeleton up (TUI MVP lands in Phase 6)");
}
