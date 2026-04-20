//! Broadcast-based per-channel message bus (gap G9 / P4.3).
//!
//! Replaces `MessageBus` (sync, `&mut self`, requires external
//! `Arc<Mutex<>>`) with a `tokio::sync::broadcast`-backed
//! pub/sub fabric:
//!
//!   * one `broadcast::Sender<BusMessage>` per channel name,
//!   * shared-state lookup via `std::sync::Mutex<HashMap<…>>` (only
//!     the registry mutates; the channels themselves are lock-free),
//!   * bounded per-channel capacity → automatic back-pressure: lagged
//!     subscribers see `RecvError::Lagged(N)` and resync,
//!   * publish is `&self` — no caller-side `Arc<Mutex<MessageBus>>`,
//!   * publish to a channel with no subscribers is a silent drop
//!     (publisher does not block, mirroring tokio broadcast semantics).
//!
//! `BusMessage` is intentionally identical to `workers::BusMessage` so
//! callers can migrate one channel at a time without changing payload
//! shapes.
//!
//! Wiring `BroadcastBus` into `workers::TaskDAG` / agent fan-out is a
//! separate, opt-in step done by the orchestrator owner — this module
//! only owns the new transport.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusMessage {
    pub from: String,
    pub content: String,
    pub timestamp: u64,
    pub channel: String,
}

/// Default per-channel buffer capacity. 256 strikes a balance: large
/// enough that a UI subscriber that briefly stalls (e.g. paint frame)
/// won't lag, small enough that an unread channel isn't pinning
/// memory.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("publish to channel `{0}` had no subscribers")]
    NoSubscribers(String),
}

/// Lock-free-on-the-hot-path broadcast bus. The internal `Mutex` is
/// held only while looking up / inserting a channel sender, NOT
/// during publish or subscribe (those return clones of the
/// `Sender`).
#[derive(Clone, Default)]
pub struct BroadcastBus {
    inner: Arc<Mutex<HashMap<String, broadcast::Sender<BusMessage>>>>,
    capacity: usize,
}

impl std::fmt::Debug for BroadcastBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BroadcastBus")
            .field("capacity", &self.capacity)
            .field(
                "channel_count",
                &self
                    .inner
                    .lock()
                    .map(|m| m.len())
                    .unwrap_or(0),
            )
            .finish()
    }
}

impl BroadcastBus {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CHANNEL_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            capacity: capacity.max(1),
        }
    }

    /// Subscribe to `channel`. The first subscribe creates the
    /// underlying broadcast channel; subsequent subscribes get a fresh
    /// `Receiver` against the same `Sender`. Receivers see only
    /// messages published AFTER they subscribe (this is the standard
    /// broadcast semantic — by design).
    pub fn subscribe(&self, channel: &str) -> broadcast::Receiver<BusMessage> {
        let mut map = self.inner.lock().expect("BroadcastBus mutex poisoned");
        let sender = map
            .entry(channel.to_string())
            .or_insert_with(|| broadcast::channel::<BusMessage>(self.capacity).0);
        sender.subscribe()
    }

    /// Publish a message to its `message.channel`. Returns:
    ///   * `Ok(n)` — n active receivers received the message.
    ///   * `Err(BusError::NoSubscribers)` — channel exists but no live
    ///     receivers (or channel doesn't exist at all). The message is
    ///     dropped — broadcast does not buffer for future subscribers.
    ///
    /// Lagged receivers do NOT cause an error here; they will see
    /// `RecvError::Lagged(N)` on their next `recv` call. That is the
    /// intended back-pressure path (caller resyncs from canonical
    /// state).
    pub fn publish(&self, message: BusMessage) -> Result<usize, BusError> {
        let sender = {
            let map = self.inner.lock().expect("BroadcastBus mutex poisoned");
            map.get(&message.channel).cloned()
        };
        let Some(sender) = sender else {
            return Err(BusError::NoSubscribers(message.channel));
        };
        let channel = message.channel.clone();
        match sender.send(message) {
            Ok(n) => Ok(n),
            // `send` errors only when there are zero active receivers.
            // The Sender is kept alive in our registry, so this is the
            // "active subscribers all dropped" case; treat same as
            // never-existed for the caller.
            Err(_) => Err(BusError::NoSubscribers(channel)),
        }
    }

    /// Total number of channels currently registered (regardless of
    /// whether they have live subscribers).
    pub fn channel_count(&self) -> usize {
        self.inner
            .lock()
            .expect("BroadcastBus mutex poisoned")
            .len()
    }

    /// Active receiver count for `channel`. 0 if the channel doesn't
    /// exist or all subscribers have dropped.
    pub fn receiver_count(&self, channel: &str) -> usize {
        self.inner
            .lock()
            .expect("BroadcastBus mutex poisoned")
            .get(channel)
            .map(|s| s.receiver_count())
            .unwrap_or(0)
    }

    /// Drop the channel registration. Live receivers will see
    /// `RecvError::Closed` after consuming any buffered messages.
    /// Returns `true` if the channel existed.
    pub fn close(&self, channel: &str) -> bool {
        self.inner
            .lock()
            .expect("BroadcastBus mutex poisoned")
            .remove(channel)
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn msg(from: &str, content: &str, channel: &str) -> BusMessage {
        BusMessage {
            from: from.into(),
            content: content.into(),
            timestamp: 0,
            channel: channel.into(),
        }
    }

    #[tokio::test]
    async fn publish_to_subscriber_delivers() {
        let bus = BroadcastBus::new();
        let mut rx = bus.subscribe("ch");
        let n = bus.publish(msg("a", "hi", "ch")).unwrap();
        assert_eq!(n, 1);
        let got = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.content, "hi");
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_errs() {
        let bus = BroadcastBus::new();
        // Register channel via subscribe then drop the receiver.
        {
            let _ = bus.subscribe("ch");
        }
        let err = bus.publish(msg("a", "x", "ch")).unwrap_err();
        match err {
            BusError::NoSubscribers(c) => assert_eq!(c, "ch"),
        }
    }

    #[tokio::test]
    async fn publish_to_unknown_channel_errs() {
        let bus = BroadcastBus::new();
        let err = bus.publish(msg("a", "x", "missing")).unwrap_err();
        assert!(matches!(err, BusError::NoSubscribers(_)));
    }

    #[tokio::test]
    async fn fanout_to_multiple_subscribers() {
        let bus = BroadcastBus::new();
        let mut r1 = bus.subscribe("ch");
        let mut r2 = bus.subscribe("ch");
        let n = bus.publish(msg("a", "hello", "ch")).unwrap();
        assert_eq!(n, 2);
        assert_eq!(r1.recv().await.unwrap().content, "hello");
        assert_eq!(r2.recv().await.unwrap().content, "hello");
    }

    #[tokio::test]
    async fn lagged_receiver_sees_lagged_error_then_resyncs() {
        let bus = BroadcastBus::with_capacity(2);
        let mut rx = bus.subscribe("ch");
        // Fill capacity + 1 → oldest should be dropped, lag flagged
        // on next recv.
        for i in 0..5 {
            bus.publish(msg("a", &format!("m{i}"), "ch")).unwrap();
        }
        let first = rx.recv().await;
        assert!(matches!(
            first,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_))
        ));
        // Subsequent recvs deliver the most recent buffered messages.
        let next = rx.recv().await.unwrap();
        // We dropped 3 oldest; the next available is m3.
        assert_eq!(next.content, "m3");
    }

    #[tokio::test]
    async fn channel_count_and_receiver_count_track_state() {
        let bus = BroadcastBus::new();
        assert_eq!(bus.channel_count(), 0);
        let _r1 = bus.subscribe("a");
        let _r2 = bus.subscribe("a");
        let _r3 = bus.subscribe("b");
        assert_eq!(bus.channel_count(), 2);
        assert_eq!(bus.receiver_count("a"), 2);
        assert_eq!(bus.receiver_count("b"), 1);
        assert_eq!(bus.receiver_count("missing"), 0);
    }

    #[tokio::test]
    async fn close_evicts_channel_and_signals_receivers() {
        let bus = BroadcastBus::new();
        let mut rx = bus.subscribe("ch");
        assert!(bus.close("ch"));
        // Closed channel: recv eventually returns Closed.
        let res = rx.recv().await;
        assert!(matches!(
            res,
            Err(tokio::sync::broadcast::error::RecvError::Closed)
        ));
        // Re-publishing the same channel name now errs (sender gone).
        let err = bus.publish(msg("a", "x", "ch")).unwrap_err();
        assert!(matches!(err, BusError::NoSubscribers(_)));
    }

    #[tokio::test]
    async fn publish_is_concurrent_safe() {
        // Multiple publishers + subscribers under tokio without an
        // external Mutex around BroadcastBus.
        let bus = BroadcastBus::new();
        let mut rx = bus.subscribe("ch");
        let bus2 = bus.clone();
        let bus3 = bus.clone();
        let h1 = tokio::spawn(async move {
            for i in 0..50 {
                let _ = bus2.publish(msg("p1", &format!("a{i}"), "ch"));
            }
        });
        let h2 = tokio::spawn(async move {
            for i in 0..50 {
                let _ = bus3.publish(msg("p2", &format!("b{i}"), "ch"));
            }
        });
        let _ = h1.await;
        let _ = h2.await;
        // Drain whatever made it through; we don't assert exact
        // ordering — just that recv doesn't deadlock and receives
        // at least one message.
        let mut count = 0;
        while let Ok(Ok(_)) =
            tokio::time::timeout(Duration::from_millis(50), rx.recv()).await
        {
            count += 1;
            if count > 200 {
                break;
            }
        }
        assert!(count > 0, "broadcast bus delivered nothing under contention");
    }

    #[test]
    fn busmessage_serde_roundtrip() {
        let m = BusMessage {
            from: "a".into(),
            content: "hi".into(),
            timestamp: 42,
            channel: "ch".into(),
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: BusMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
}
