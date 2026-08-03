use arbe_core::RuntimeEvent;
use tokio::sync::broadcast;

/// Broadcasts `RuntimeEvent`s to any number of subscribers (the TUI, loggers,
/// future clients) without those subscribers coupling to loop internals
/// (overall design §2.1, TUI spec §5).
pub struct EventBus {
    sender: broadcast::Sender<RuntimeEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.sender.subscribe()
    }

    /// Drops the event if there are no subscribers; the loop itself must
    /// never block or fail because nobody is listening.
    pub fn publish(&self, event: RuntimeEvent) {
        let _ = self.sender.send(event);
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::SessionId;

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let bus = EventBus::default();
        let mut rx = bus.subscribe();
        let session_id = SessionId::new();
        bus.publish(RuntimeEvent::SessionStarted { session_id });
        let received = rx.recv().await.unwrap();
        match received {
            RuntimeEvent::SessionStarted { session_id: got } => assert_eq!(got, session_id),
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
