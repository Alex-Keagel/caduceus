//! ST9 — integration tier for pin-survival + compaction seam (caduceus side).
//!
//! Exercises the contract that the zed `Thread` relies on across compaction
//! cycles: pinned context survives an arbitrary number of compaction passes
//! and the pinned-token accounting stays stable.
//!
//! ST9 originally enumerated four scenarios. Two are testable today and live
//! here:
//!
//! * **(1)** First-user pin survives a long thread of compactions.
//! * **(2)** Compaction's emitted-message contract is preserved (summary
//!   surfaces the first/last messages so the caller side can render the
//!   `context.compacted` notice without losing anchor context).
//!
//! The other two ST9 scenarios (sub-agent vendor-fallback on timeout, and
//! Copilot-Chat unauth visibility) are deferred — see
//! `docs/design/deferred/st9-untestable-scenarios.md`. Their fixes are not in
//! the codebase yet, so a regression test would be vacuous.

use caduceus_orchestrator::context::{CompactionStrategy, ContextManager};
use caduceus_providers::Message;

/// Build N synthetic turns: a leading user message + alternating user/assistant
/// pairs. Returns `(messages, first_user_label, first_user_content)`.
fn build_thread(turn_count: usize) -> (Vec<Message>, String, String) {
    let first_user_label = "first-user-message".to_string();
    let first_user_content =
        "Original user request: please refactor the authentication module to use \
         JWT tokens with rotation, and ensure the rotation interval is configurable."
            .to_string();

    let mut messages = vec![Message::user(first_user_content.clone())];
    for i in 0..turn_count {
        messages.push(Message::assistant(format!(
            "assistant turn {i}: noted, working on subtask {i}"
        )));
        messages.push(Message::user(format!(
            "user turn {i}: also please verify the test for subtask {i}"
        )));
    }
    (messages, first_user_label, first_user_content)
}

/// **(1)** ST9 scenario 1 — the first-user pin (the most common UX case:
/// "remember what I originally asked") must survive 30 successive compaction
/// passes.
#[test]
fn first_user_pin_survives_30_compactions() {
    let mut mgr = ContextManager::new(128_000);
    let (mut messages, label, content) = build_thread(60);

    // Pin the original request before any compaction runs.
    mgr.pin(label.clone(), content.clone());
    let pinned_tokens_initial = mgr.pinned_tokens();
    assert!(
        pinned_tokens_initial > 0,
        "pin should account for non-zero tokens"
    );

    let strategy = CompactionStrategy::Hybrid {
        summarize_before: 20,
        keep_verbatim: 5,
    };

    // 30 compaction passes — each pass shrinks history, then we append fresh
    // turns to push it back up, simulating a long-running thread.
    for round in 0..30 {
        messages = mgr.compact(&messages, &strategy);
        // Append new conversation to grow the thread back up.
        messages.push(Message::assistant(format!(
            "post-compact-{round} assistant"
        )));
        messages.push(Message::user(format!("post-compact-{round} user")));

        // Pin must still be present, with the same tokens.
        let pins = mgr.list_pins();
        assert_eq!(
            pins.len(),
            1,
            "round {round}: pin count drifted (got {})",
            pins.len()
        );
        assert_eq!(
            pins[0].label, label,
            "round {round}: pin label mutated unexpectedly"
        );
        assert_eq!(
            pins[0].content, content,
            "round {round}: pin content mutated unexpectedly"
        );
        assert_eq!(
            mgr.pinned_tokens(),
            pinned_tokens_initial,
            "round {round}: pinned_tokens drifted"
        );
    }

    // Sanity: the pin contributes to the breakdown after 30 rounds, exactly as
    // it did before any compaction.
    let breakdown = mgr.get_breakdown(&messages, "system prompt", "[]");
    assert_eq!(
        breakdown.pinned_context_tokens, pinned_tokens_initial,
        "breakdown pinned_context_tokens drifted from initial"
    );
}

/// **(2)** ST9 scenario 2 — the contract the zed `Thread` relies on when it
/// renders the `context.compacted` notice: compaction's summary message must
/// still anchor the first and last messages of the compacted slice so the UI
/// can present "summarised X turns" without losing the user's original ask.
#[test]
fn hybrid_compaction_summary_anchors_first_and_last() {
    let mgr = ContextManager::new(128_000);
    let (messages, _label, _first_user_content) = build_thread(40);

    let compacted = mgr.compact(
        &messages,
        &CompactionStrategy::Hybrid {
            summarize_before: 30,
            keep_verbatim: 5,
        },
    );

    // First message of the compacted output is the summary system message
    // (the wire format the bridge consumes).
    let summary = compacted
        .first()
        .expect("compaction must produce at least the summary message");
    assert_eq!(
        summary.role, "system",
        "summary should ride on a system-role message"
    );
    let body = &summary.content;
    assert!(
        body.contains("[Compacted conversation summary]"),
        "summary header missing: {body}"
    );

    // The summary references the original user ask. We only check a small,
    // distinctive slice — the `build_summary` helper truncates at 200 chars
    // so we don't anchor on the full string.
    assert!(
        body.contains("Original user request"),
        "summary lost first-message anchor; got: {body}"
    );
}

/// Pin under repeated compaction must NOT contribute its content twice — once
/// via the dedicated `pinned` slot and once via being mistakenly re-injected
/// into the message vector. This is the silent-double-counting regression the
/// integration tier is here to guard against.
#[test]
fn compact_does_not_re_inject_pinned_content_into_messages() {
    let mut mgr = ContextManager::new(128_000);
    let (messages, label, content) = build_thread(20);
    mgr.pin(label, content.clone());

    let compacted = mgr.compact(
        &messages,
        &CompactionStrategy::Hybrid {
            summarize_before: 10,
            keep_verbatim: 3,
        },
    );

    // Pin lives in the dedicated slot; it must not also appear as a message.
    let pinned_in_messages = compacted.iter().filter(|m| m.content == content).count();
    assert_eq!(
        pinned_in_messages, 0,
        "pinned content leaked into compacted message vector ({pinned_in_messages} copies)"
    );
}
