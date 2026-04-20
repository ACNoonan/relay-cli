//! Central event bus for `bridge` worker → subscribers.
//!
//! ## Why this exists
//!
//! Before this module the `Worker` owned a single `mpsc::Sender<WorkerEvent>` and the
//! `tui_chat` UI owned the matching receiver. Adding a *second* observer (a telemetry
//! sink, an alternative UI, a skill trigger, …) meant either threading another sender
//! through `WorkerConfig` or fanning the events out manually downstream of the worker.
//!
//! The `EventBus` collapses that into one type: every emit is broadcast to every
//! current subscriber, and `subscribe()` is a one-line addition for new observers.
//!
//! ## What it is
//!
//! A trivially thin wrapper around [`tokio::sync::broadcast`]. The bus is **not** a
//! generic pub/sub — there is exactly one event type ([`BridgeEvent`], a re-export of
//! [`super::worker::WorkerEvent`]) and every subscriber sees every event. Filtering
//! is the subscriber's job.
//!
//! ## What it isn't
//!
//! * Not a replacement for the per-backend MPSC channels that feed events into the
//!   worker. Those are 1:1 streams and gain nothing from broadcasting.
//! * Not a place to add cross-process or persistent delivery — `broadcast` is a
//!   single-process, in-memory bus. If a subscriber lags it loses events (with a
//!   logged warn). That's a deliberate fit for a UI bus, where missing a single
//!   transient status line is fine but blocking the worker on a slow subscriber is
//!   not.
//!
//! ## Capacity
//!
//! Each subscriber gets a bounded queue of `capacity` events. The chat TUI is the
//! only subscriber today; we pick `256` as a comfortable headroom — typical worker
//! emissions are 1–10 per turn, so 256 covers many turns of UI stalls (e.g. the
//! user dragging the terminal). When a subscriber's queue fills, `recv()` returns
//! [`broadcast::error::RecvError::Lagged(n)`]; we log and continue, which manifests
//! to the user as a brief delay before the next conversation snapshot lands.

use tokio::sync::broadcast;

/// The single event type carried on the bus.
///
/// Kept as a re-export of [`super::worker::WorkerEvent`] (Option A from the design
/// brief): `WorkerEvent` is already `Clone` and the variants already cover every
/// observable outcome the worker produces. A future requirement for non-worker
/// events (telemetry, persistence acks) can be added either as new variants or as
/// a wrapper enum without changing `EventBus` itself.
pub type BridgeEvent = super::worker::WorkerEvent;

/// Cloneable handle to a `tokio::sync::broadcast` channel scoped to bridge events.
///
/// `Clone` is mandatory — `broadcast::Sender` is internally an `Arc`, so cloning
/// the bus just hands out another reference to the same underlying channel.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<BridgeEvent>,
}

impl EventBus {
    /// Create a bus where each subscriber buffers up to `capacity` events.
    ///
    /// Capacity is per-subscriber, not global — a slow subscriber doesn't starve
    /// fast ones. See the module docs on capacity tuning.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Subscribe to all subsequent events. Drop the returned receiver to
    /// unsubscribe; the bus tracks active receivers internally.
    pub fn subscribe(&self) -> broadcast::Receiver<BridgeEvent> {
        self.tx.subscribe()
    }

    /// Emit one event to every current subscriber.
    ///
    /// Returns nothing: lagging subscribers are not the emitter's problem (they'll
    /// observe a `Lagged(n)` on their next `recv`). When there are zero subscribers
    /// `broadcast::Sender::send` returns `Err`; we log it at trace and discard,
    /// matching the previous "drop on closed channel" behaviour of the MPSC sender.
    pub fn emit(&self, event: BridgeEvent) {
        if let Err(err) = self.tx.send(event) {
            // SendError fires only when there are zero active receivers. That's
            // not pathological — it just means the UI hasn't subscribed yet (or
            // has shut down). Log at trace to keep the worker quiet.
            tracing::trace!(?err, "EventBus::emit dropped (no active subscribers)");
        }
    }

    /// Number of currently active subscribers. Useful for tests; not load-bearing
    /// in production.
    #[allow(dead_code)]
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::conversation::{Agent, Conversation};
    use crate::bridge::worker::{WorkerEvent, WorkerStatus};
    use tokio::sync::broadcast::error::RecvError;

    fn sample_status_msg(s: &str) -> BridgeEvent {
        WorkerEvent::StatusMessage(s.to_string())
    }

    #[tokio::test]
    async fn emit_then_subscribe_round_trip() {
        // Note: subscribe BEFORE emit — broadcast only delivers events that occur
        // after a receiver is created. This mirrors how the bus is wired up in
        // `tui_chat::run`: the UI subscribes before the worker emits its first
        // snapshot.
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        bus.emit(sample_status_msg("hello"));
        match rx.recv().await {
            Ok(WorkerEvent::StatusMessage(m)) => assert_eq!(m, "hello"),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn multiple_subscribers_each_receive_every_event() {
        let bus = EventBus::new(8);
        let mut rx_a = bus.subscribe();
        let mut rx_b = bus.subscribe();
        bus.emit(sample_status_msg("one"));
        bus.emit(WorkerEvent::StatusChanged(WorkerStatus::Idle));

        // rx_a sees both
        match rx_a.recv().await.unwrap() {
            WorkerEvent::StatusMessage(m) => assert_eq!(m, "one"),
            other => panic!("rx_a first: {other:?}"),
        }
        match rx_a.recv().await.unwrap() {
            WorkerEvent::StatusChanged(WorkerStatus::Idle) => {}
            other => panic!("rx_a second: {other:?}"),
        }

        // rx_b sees both
        match rx_b.recv().await.unwrap() {
            WorkerEvent::StatusMessage(m) => assert_eq!(m, "one"),
            other => panic!("rx_b first: {other:?}"),
        }
        match rx_b.recv().await.unwrap() {
            WorkerEvent::StatusChanged(WorkerStatus::Idle) => {}
            other => panic!("rx_b second: {other:?}"),
        }
    }

    #[tokio::test]
    async fn lagging_subscriber_gets_lagged_then_resumes_at_most_recent() {
        // capacity = 2 so we can overflow easily.
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();
        // Push more events than the subscriber's queue can hold without ever
        // draining. After capacity is exceeded, the broadcast channel discards
        // the oldest events and the next `recv` surfaces a Lagged(n).
        bus.emit(sample_status_msg("a"));
        bus.emit(sample_status_msg("b"));
        bus.emit(sample_status_msg("c"));
        bus.emit(sample_status_msg("d"));

        // First recv should be Lagged. The exact `n` depends on tokio internals
        // (typically `count - capacity`), so we just assert *some* lag was
        // reported.
        match rx.recv().await {
            Err(RecvError::Lagged(n)) => assert!(n >= 1, "lagged count was {n}"),
            other => panic!("expected Lagged, got {other:?}"),
        }

        // After a Lagged the receiver is auto-advanced to the oldest event still
        // in the queue (NOT necessarily the newest). With capacity 2 and 4
        // emits, after the lag we should see "c" then "d".
        let next = match rx.recv().await {
            Ok(WorkerEvent::StatusMessage(m)) => m,
            other => panic!("after lag, expected StatusMessage, got {other:?}"),
        };
        assert_eq!(next, "c");
        let last = match rx.recv().await {
            Ok(WorkerEvent::StatusMessage(m)) => m,
            other => panic!("after lag, expected second StatusMessage, got {other:?}"),
        };
        assert_eq!(last, "d");
    }

    #[tokio::test]
    async fn closed_when_all_receivers_dropped() {
        // Confirms that emitting with zero receivers is harmless (logged at
        // trace, swallowed). This is the path exercised when the UI shuts down
        // before the worker.
        let bus = EventBus::new(4);
        let rx = bus.subscribe();
        drop(rx);
        // Should not panic / not block.
        bus.emit(sample_status_msg("orphan"));
        assert_eq!(bus.receiver_count(), 0);
    }

    #[tokio::test]
    async fn carries_a_full_conversation_snapshot() {
        // Smoke test that the bus moves the heavier `ConversationUpdated`
        // variant intact — `Conversation` is a non-trivial owned struct.
        let bus = EventBus::new(4);
        let mut rx = bus.subscribe();
        let conv = Conversation::new(Agent::Claude, true);
        let id = conv.id;
        bus.emit(WorkerEvent::ConversationUpdated(conv));
        match rx.recv().await.unwrap() {
            WorkerEvent::ConversationUpdated(c) => assert_eq!(c.id, id),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
