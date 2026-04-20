//! P3.4 — Background notification fabric (gap G15).
//!
//! When a background workload (an automation, a long‑running headless
//! task, an MCP server reload, etc.) finishes, the IDE shell needs to
//! surface a *toast* — even if the user has switched away from the
//! agent panel. Existing telemetry (OTel spans, `AgentEvent`s) is
//! ill‑suited because it is scoped to a live session; an automation
//! triggered by cron has no panel attached at start.
//!
//! This module sits on top of [`crate::broadcast_bus::BroadcastBus`]
//! and provides:
//!
//!   * a stable channel name [`NOTIFICATIONS_CHANNEL`] that the bridge
//!     subscribes to on startup,
//!   * a typed [`Notification`] envelope (severity + body) that
//!     serialises identically across IPC,
//!   * a one‑line publisher [`publish_automation_completion`] for the
//!     `automations.rs` runner — call it after `AutomationRunner::run`
//!     to fan a toast out to every live UI surface.
//!
//! No `&mut`, no extra mutex — `BroadcastBus::publish` is `&self` and
//! lock‑free on the hot path. A publish to a channel with zero
//! subscribers is a silent drop, matching tokio broadcast semantics
//! (the cron fired, nobody was listening — that's fine).
//!
//! The bridge layer subscribes once with [`subscribe`] and drains via
//! its async receiver into the React panel.

use crate::automations::AutomationResult;
use crate::broadcast_bus::{BroadcastBus, BusError, BusMessage};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::Receiver;

/// Stable channel name. The bridge MUST use this constant rather than
/// a string literal so a future rename surfaces as a compile error.
pub const NOTIFICATIONS_CHANNEL: &str = "notifications";

/// Severity drives the toast colour in the UI. Mapped 1:1 with
/// VS Code / Zed notification levels so the bridge can forward
/// without translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// Typed notification body. Serialised into [`BusMessage::content`] as
/// JSON so the bridge can deserialise without owning the orchestrator
/// crate transitively (only the JSON shape is stable contract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub severity: Severity,
    pub title: String,
    pub body: String,
    /// Source identifier (e.g. `"automation:nightly-test"`,
    /// `"headless:eval-2025-01-12"`). Lets the panel deduplicate or
    /// group by source.
    pub source: String,
}

impl Notification {
    pub fn info(source: impl Into<String>, title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            source: source.into(),
            title: title.into(),
            body: body.into(),
        }
    }

    pub fn error(source: impl Into<String>, title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            source: source.into(),
            title: title.into(),
            body: body.into(),
        }
    }
}

/// Subscribe to the notifications channel. Always succeeds; the first
/// call also creates the underlying broadcast channel.
pub fn subscribe(bus: &BroadcastBus) -> Receiver<BusMessage> {
    bus.subscribe(NOTIFICATIONS_CHANNEL)
}

/// Publish a typed [`Notification`] to the notifications channel.
/// Returns the number of receivers that got the message, or
/// [`BusError::NoSubscribers`] when nobody is listening (silent drop).
/// The notification is JSON‑encoded into [`BusMessage::content`]; the
/// bridge `serde_json::from_str` to recover the typed shape.
pub fn publish(bus: &BroadcastBus, n: Notification) -> Result<usize, BusError> {
    let json = serde_json::to_string(&n)
        .expect("Notification serializes (no non-UTF8 / cycles by construction)");
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    bus.publish(BusMessage {
        from: n.source.clone(),
        content: json,
        timestamp,
        channel: NOTIFICATIONS_CHANNEL.to_string(),
    })
}

/// Sugar for the most common publisher: an automation just finished.
/// Translates [`AutomationResult`] into a [`Notification`] and pushes
/// it. Failed runs become [`Severity::Error`]; successful runs are
/// [`Severity::Info`].
pub fn publish_automation_completion(
    bus: &BroadcastBus,
    result: &AutomationResult,
) -> Result<usize, BusError> {
    let n = if result.success {
        Notification::info(
            format!("automation:{}", result.automation_id),
            format!("Automation '{}' completed", result.automation_id),
            result.output.clone(),
        )
    } else {
        Notification::error(
            format!("automation:{}", result.automation_id),
            format!("Automation '{}' failed", result.automation_id),
            result.output.clone(),
        )
    };
    publish(bus, n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automations::AutomationResult;
    use chrono::Utc;
    use tokio::time::{Duration, timeout};

    fn dummy_result(success: bool) -> AutomationResult {
        AutomationResult {
            automation_id: "nightly-test".into(),
            trigger_event: "cron:0 2 * * *".into(),
            started_at: Utc::now(),
            completed_at: Utc::now(),
            success,
            output: "ran 12 tests".into(),
            tokens_used: Default::default(),
            cost_usd: 0.0,
            commit_sha: None,
            pr_url: None,
        }
    }

    #[tokio::test]
    async fn p3_4_subscribe_then_publish_delivers() {
        let bus = BroadcastBus::new();
        let mut rx = subscribe(&bus);
        let n = Notification::info("test", "hi", "body");
        let delivered = publish(&bus, n.clone()).unwrap();
        assert_eq!(delivered, 1);
        let msg = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("recv timeout")
            .expect("recv error");
        assert_eq!(msg.channel, NOTIFICATIONS_CHANNEL);
        let parsed: Notification = serde_json::from_str(&msg.content).unwrap();
        assert_eq!(parsed, n);
    }

    #[tokio::test]
    async fn p3_4_publish_without_subscribers_is_silent_drop() {
        let bus = BroadcastBus::new();
        let n = Notification::info("test", "x", "y");
        let err = publish(&bus, n).unwrap_err();
        assert!(matches!(err, BusError::NoSubscribers(_)));
    }

    #[tokio::test]
    async fn p3_4_automation_success_maps_to_info_severity() {
        let bus = BroadcastBus::new();
        let mut rx = subscribe(&bus);
        publish_automation_completion(&bus, &dummy_result(true)).unwrap();
        let msg = timeout(Duration::from_millis(100), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let n: Notification = serde_json::from_str(&msg.content).unwrap();
        assert_eq!(n.severity, Severity::Info);
        assert!(n.title.contains("completed"));
        assert_eq!(n.source, "automation:nightly-test");
    }

    #[tokio::test]
    async fn p3_4_automation_failure_maps_to_error_severity() {
        let bus = BroadcastBus::new();
        let mut rx = subscribe(&bus);
        publish_automation_completion(&bus, &dummy_result(false)).unwrap();
        let msg = timeout(Duration::from_millis(100), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let n: Notification = serde_json::from_str(&msg.content).unwrap();
        assert_eq!(n.severity, Severity::Error);
        assert!(n.title.contains("failed"));
    }

    #[tokio::test]
    async fn p3_4_multiple_subscribers_each_receive_message() {
        let bus = BroadcastBus::new();
        let mut rx1 = subscribe(&bus);
        let mut rx2 = subscribe(&bus);
        publish(&bus, Notification::info("s", "t", "b")).unwrap();
        let m1 = timeout(Duration::from_millis(100), rx1.recv()).await.unwrap().unwrap();
        let m2 = timeout(Duration::from_millis(100), rx2.recv()).await.unwrap().unwrap();
        assert_eq!(m1.content, m2.content);
    }

    #[test]
    fn p3_4_severity_serializes_snake_case() {
        let s = serde_json::to_string(&Severity::Warning).unwrap();
        assert_eq!(s, "\"warning\"");
    }
}
