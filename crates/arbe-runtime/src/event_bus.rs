use std::sync::Mutex;

use arbe_core::{EventEnvelope, RuntimeEvent};
use tokio::sync::broadcast;

/// Broadcasts `RuntimeEvent`s to any number of subscribers (the TUI, loggers,
/// future clients) without those subscribers coupling to loop internals
/// (overall design §2.1, TUI spec §5). Each event is wrapped in an
/// [`EventEnvelope`] with a gap-free sequence number.
pub struct EventBus {
    sender: broadcast::Sender<EventEnvelope>,
    /// Next sequence number. A mutex rather than an atomic so that taking a
    /// number and sending happen together: two concurrent publishers can't
    /// deliver `seq` values out of order.
    next_seq: Mutex<u64>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            sender,
            next_seq: Mutex::new(0),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.sender.subscribe()
    }

    /// Drops the event if there are no subscribers; the loop itself must
    /// never block or fail because nobody is listening. The sequence number
    /// advances either way, so it counts events *published*, not delivered.
    pub fn publish(&self, event: RuntimeEvent) {
        let mut next_seq = self.next_seq.lock().unwrap_or_else(|p| p.into_inner());
        let seq = *next_seq;
        *next_seq += 1;
        let _ = self.sender.send(EventEnvelope { seq, event });
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
        match received.event {
            RuntimeEvent::SessionStarted { session_id: got } => assert_eq!(got, session_id),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn sequence_numbers_are_consecutive_and_count_unobserved_events() {
        let bus = EventBus::default();
        let session_id = SessionId::new();
        // Published before anyone subscribed: dropped, but still numbered.
        bus.publish(RuntimeEvent::SessionStarted { session_id });
        let mut rx = bus.subscribe();
        bus.publish(RuntimeEvent::SessionStarted { session_id });
        bus.publish(RuntimeEvent::SessionStarted { session_id });
        assert_eq!(rx.recv().await.unwrap().seq, 1);
        assert_eq!(rx.recv().await.unwrap().seq, 2);
    }
}
