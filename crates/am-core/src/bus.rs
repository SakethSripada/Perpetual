use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use am_proto::{AppEvent, EventReplay, SequencedEvent};
use tokio::sync::broadcast;

/// How many recent events the bus retains for replay. Sized so a UI or daemon
/// client that lags briefly (or reconnects) can catch up without a full state
/// refetch; beyond this window consumers fall back to refetching.
const RING_CAPACITY: usize = 8192;

/// In-process event bus. The orchestrator publishes [`AppEvent`]s; the Tauri
/// shell or daemon subscribes and forwards them to the UI. Decoupling here is
/// what keeps the core UI-agnostic.
///
/// Every event carries a gap-free sequence number and is retained in a bounded
/// ring, so a consumer that observes a lag (or reconnects) can replay exactly
/// the events it missed via [`EventBus::replay_since`] instead of silently
/// dropping updates.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<BusInner>,
}

struct BusInner {
    tx: broadcast::Sender<SequencedEvent>,
    /// Guards sequence assignment, ring insertion, and broadcast send so the
    /// broadcast order always matches the sequence order.
    state: Mutex<RingState>,
}

struct RingState {
    next_seq: u64,
    ring: VecDeque<SequencedEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(1024);
        Self {
            inner: Arc::new(BusInner {
                tx,
                state: Mutex::new(RingState {
                    next_seq: 0,
                    ring: VecDeque::with_capacity(256),
                }),
            }),
        }
    }

    /// Subscribe to the live event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<SequencedEvent> {
        self.inner.tx.subscribe()
    }

    /// Publish an event. Errors (no active subscribers) are intentionally
    /// ignored — events are fire-and-forget for live UI; replay covers laggards.
    pub fn publish(&self, event: AppEvent) {
        let mut state = self.inner.state.lock().unwrap();
        state.next_seq += 1;
        let sequenced = SequencedEvent {
            seq: state.next_seq,
            event,
        };
        if state.ring.len() >= RING_CAPACITY {
            state.ring.pop_front();
        }
        state.ring.push_back(sequenced.clone());
        let _ = self.inner.tx.send(sequenced);
    }

    /// The sequence number of the most recently published event (0 if none).
    pub fn latest_seq(&self) -> u64 {
        self.inner.state.lock().unwrap().next_seq
    }

    /// Events published after `since_seq`, oldest first. `complete` is false
    /// when the requested range fell off the ring — the caller missed events
    /// that can no longer be replayed and should refetch state.
    pub fn replay_since(&self, since_seq: u64) -> EventReplay {
        let state = self.inner.state.lock().unwrap();
        let oldest_retained = state.ring.front().map(|event| event.seq);
        let complete = match oldest_retained {
            // Retention starts at or before the first event the caller needs.
            Some(oldest) => oldest <= since_seq + 1,
            None => state.next_seq == since_seq,
        };
        let events = state
            .ring
            .iter()
            .filter(|event| event.seq > since_seq)
            .cloned()
            .collect();
        EventReplay {
            complete,
            latest_seq: state.next_seq,
            events,
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use am_proto::ActivityEvent;

    fn event(kind: &str) -> AppEvent {
        AppEvent::Activity(ActivityEvent {
            id: kind.to_string(),
            project_id: None,
            task_id: None,
            kind: kind.to_string(),
            payload: serde_json::Value::Null,
            ts: am_proto::now(),
        })
    }

    #[test]
    fn sequences_are_monotonic_and_gap_free() {
        let bus = EventBus::new();
        for i in 0..5 {
            bus.publish(event(&format!("e{i}")));
        }
        assert_eq!(bus.latest_seq(), 5);
        let replay = bus.replay_since(0);
        assert!(replay.complete);
        let seqs: Vec<u64> = replay.events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn replay_since_returns_only_missed_events() {
        let bus = EventBus::new();
        for i in 0..5 {
            bus.publish(event(&format!("e{i}")));
        }
        let replay = bus.replay_since(3);
        assert!(replay.complete);
        assert_eq!(
            replay.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![4, 5]
        );

        // Fully caught up: empty but complete.
        let caught_up = bus.replay_since(5);
        assert!(caught_up.complete);
        assert!(caught_up.events.is_empty());
    }

    #[test]
    fn replay_reports_incomplete_when_range_fell_off_ring() {
        let bus = EventBus::new();
        for i in 0..(RING_CAPACITY + 10) {
            bus.publish(event(&format!("e{i}")));
        }
        let replay = bus.replay_since(2);
        assert!(!replay.complete, "seq 3..10 are no longer retained");
        assert_eq!(replay.events.len(), RING_CAPACITY);

        let recent = bus.replay_since(bus.latest_seq() - 1);
        assert!(recent.complete);
        assert_eq!(recent.events.len(), 1);
    }

    #[tokio::test]
    async fn subscribers_receive_sequenced_events() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        bus.publish(event("hello"));
        let received = rx.recv().await.unwrap();
        assert_eq!(received.seq, 1);
        assert!(matches!(received.event, AppEvent::Activity(_)));
    }
}
