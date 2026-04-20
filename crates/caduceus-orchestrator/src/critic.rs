//! P13.6 (G‑R10.1 / G‑R10.2) — per‑turn critic loop.
//!
//! Inspired by Self‑Refine (Madaan et al., NeurIPS 2023) and CRITIC
//! (Gou et al., ICLR 2024). When the LLM signals end‑of‑turn, we
//! optionally show the candidate response to a `Critic`; on
//! `Verdict::Reject`, we append the critic's feedback as a synthetic
//! user message and loop one more turn so the model can revise.
//!
//! The harness gates the loop with `critic_max_iters` so a
//! pathologically picky critic can't burn the whole budget.
//!
//! `HeuristicCritic` is the default no‑LLM critic — it rejects
//! responses that look like obvious cop‑outs ("I cannot", "TODO",
//! suspiciously short answers). `LlmCritic` (separate type — ships
//! when a real provider is wired) calls a second model with a
//! "is this answer acceptable? reply ACCEPT or REJECT: <reason>"
//! prompt.

use async_trait::async_trait;
use caduceus_providers::Message;

/// What the critic decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Accept the candidate response — the harness emits TurnComplete
    /// and returns the answer to the user.
    Accept,
    /// Reject — the harness appends `feedback` as a synthetic user
    /// message and runs one more turn so the model can revise.
    Reject { feedback: String },
}

/// One‑shot judgement on a candidate final response. Implementors
/// SHOULD be cheap; the harness calls this synchronously inside the
/// run loop after every EndTurn.
#[async_trait]
pub trait Critic: Send + Sync {
    /// Decide whether `response` is an acceptable final answer to
    /// `task`. `history` is the full conversation up to (and
    /// including) the assistant message under review — useful for
    /// detecting "the model just repeated itself" or "ignored a tool
    /// result".
    async fn judge(&self, task: &str, response: &str, history: &[Message]) -> Verdict;
}

/// No‑LLM critic that rejects obvious cop‑outs and trivially short
/// answers. The thresholds are intentionally lenient — this is a
/// safety net, not a quality gate.
#[derive(Debug, Clone)]
pub struct HeuristicCritic {
    /// Minimum response length below which we suspect a cop‑out.
    pub min_chars: usize,
    /// Phrases that, when present in `response`, trigger a reject.
    /// Match is case‑insensitive substring.
    pub veto_phrases: Vec<String>,
}

impl Default for HeuristicCritic {
    fn default() -> Self {
        Self {
            min_chars: 12,
            veto_phrases: vec![
                "i cannot help".into(),
                "i'm unable to".into(),
                "as an ai".into(),
                // "TODO" intentionally NOT here: legitimate code
                // answers often contain TODO markers.
            ],
        }
    }
}

#[async_trait]
impl Critic for HeuristicCritic {
    async fn judge(&self, _task: &str, response: &str, _history: &[Message]) -> Verdict {
        let trimmed = response.trim();
        if trimmed.len() < self.min_chars {
            return Verdict::Reject {
                feedback: format!(
                    "Your response was {} chars long, which looks truncated or evasive. \
                     Please answer the question with concrete content.",
                    trimmed.len()
                ),
            };
        }
        let lower = trimmed.to_lowercase();
        for phrase in &self.veto_phrases {
            if lower.contains(phrase) {
                return Verdict::Reject {
                    feedback: format!(
                        "Your response contained an evasive phrase ('{}'). \
                         Please attempt the task directly using the tools available.",
                        phrase
                    ),
                };
            }
        }
        Verdict::Accept
    }
}

/// Test‑only critic that returns a programmable verdict sequence.
/// Useful for harness integration tests where we want to assert that
/// "first reject, second accept" produces exactly two LLM round‑trips.
#[derive(Debug)]
pub struct ScriptedCritic {
    verdicts: std::sync::Mutex<std::collections::VecDeque<Verdict>>,
}

impl ScriptedCritic {
    pub fn new(seq: impl IntoIterator<Item = Verdict>) -> Self {
        Self {
            verdicts: std::sync::Mutex::new(seq.into_iter().collect()),
        }
    }
}

#[async_trait]
impl Critic for ScriptedCritic {
    async fn judge(&self, _task: &str, _response: &str, _history: &[Message]) -> Verdict {
        self.verdicts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Verdict::Accept)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn p13_6_heuristic_accepts_normal_response() {
        let c = HeuristicCritic::default();
        let v = c.judge("write hello", "Hello, world! Here is the answer.", &[]).await;
        assert_eq!(v, Verdict::Accept);
    }

    #[tokio::test]
    async fn p13_6_heuristic_rejects_truncated_response() {
        let c = HeuristicCritic::default();
        match c.judge("explain x", "ok", &[]).await {
            Verdict::Reject { feedback } => assert!(feedback.contains("truncated")),
            v => panic!("expected reject, got {v:?}"),
        }
    }

    #[tokio::test]
    async fn p13_6_heuristic_rejects_evasive_phrase() {
        let c = HeuristicCritic::default();
        let response = "I cannot help with that request, sorry.";
        match c.judge("anything", response, &[]).await {
            Verdict::Reject { feedback } => assert!(feedback.contains("evasive")),
            v => panic!("expected reject, got {v:?}"),
        }
    }

    #[tokio::test]
    async fn p13_6_scripted_critic_replays_verdicts_in_order() {
        let c = ScriptedCritic::new(vec![
            Verdict::Reject {
                feedback: "try again".into(),
            },
            Verdict::Accept,
        ]);
        assert!(matches!(c.judge("t", "r1", &[]).await, Verdict::Reject { .. }));
        assert_eq!(c.judge("t", "r2", &[]).await, Verdict::Accept);
        // Underflow defaults to Accept so a misconfigured test
        // doesn't hang the harness loop.
        assert_eq!(c.judge("t", "r3", &[]).await, Verdict::Accept);
    }
}
