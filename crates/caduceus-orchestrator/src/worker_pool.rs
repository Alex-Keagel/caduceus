//! P13.14 — `open-multi-agent` worker pool integration (G‑R10.3).
//!
//! Spawns N parallel "worker" agents (each given the same prompt) and merges
//! their outputs by consensus. This is the local‑process analogue of the
//! `open-multi-agent` Node.js runtime: when claw‑code (the orchestrator) hits
//! a sub‑task that benefits from multi‑agent debate (e.g. "draft + critique +
//! test"), it can fan‑out via this tool and reduce the verdict to a single
//! [`ToolResult`].
//!
//! The tool is generic over a [`WorkerRunner`] trait so it can be tested in
//! isolation. Real deployments wire `WorkerRunner` to `BackgroundAgent` (or
//! a thin `open-multi-agent` HTTP shim).
//!
//! Cite: Chen et al., *AgentVerse: Facilitating Multi‑Agent Collaboration and
//! Exploring Emergent Behaviors* (ICLR 2024, arXiv:2308.10848).

use crate::self_consistency::{vote, SelfConsistencyVerdict};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// One worker's output. The body is opaque JSON so the consensus layer
/// (currently [`crate::self_consistency::vote`]) can canonicalise + bucket it.
#[derive(Debug, Clone)]
pub struct WorkerOutput {
    pub agent_id: String,
    pub output: Value,
}

/// Pluggable worker runner. Implementors fan‑out the prompt to N parallel
/// agents and return their (possibly heterogeneous) outputs.
#[async_trait]
pub trait WorkerRunner: Send + Sync {
    async fn run(&self, prompt: &str, n_agents: usize) -> Result<Vec<WorkerOutput>, anyhow::Error>;
}

/// Worker‑pool tool. Acceptance: fan‑out N agents, vote on output, return
/// the consensus or escalate.
pub struct WorkerPool {
    runner: Arc<dyn WorkerRunner>,
    /// Default fan‑out when input doesn't override.
    pub default_n: usize,
}

impl WorkerPool {
    pub fn new(runner: Arc<dyn WorkerRunner>) -> Self {
        Self {
            runner,
            default_n: 3,
        }
    }

    pub fn with_default_n(mut self, n: usize) -> Self {
        self.default_n = n.max(1);
        self
    }

    /// Run the pool, returning a structured result envelope.
    /// Input shape: `{"prompt": "...", "n_agents": 3}` (n_agents optional).
    pub async fn call(&self, input: &Value) -> Result<Value, anyhow::Error> {
        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required 'prompt' field"))?;
        let n_agents = input
            .get("n_agents")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(self.default_n)
            .max(1);
        let outputs = self.runner.run(prompt, n_agents).await?;
        if outputs.is_empty() {
            return Ok(json!({
                "consensus": "none",
                "reason": "no worker returned output",
                "candidates": [],
            }));
        }
        // Vote on canonicalised outputs.
        let payloads: Vec<Value> = outputs.iter().map(|o| o.output.clone()).collect();
        let verdict = vote(&payloads);
        let mut candidates = Vec::with_capacity(outputs.len());
        for o in &outputs {
            candidates.push(json!({"agent": o.agent_id, "output": o.output}));
        }
        let envelope = match verdict {
            SelfConsistencyVerdict::Quorum { winner, votes } => json!({
                "consensus": "quorum",
                "winner": winner,
                "votes": votes,
                "n_agents": outputs.len(),
                "candidates": candidates,
            }),
            SelfConsistencyVerdict::NoQuorum {
                top_candidate,
                top_votes,
            } => json!({
                "consensus": "none",
                "top_candidate": top_candidate,
                "top_votes": top_votes,
                "n_agents": outputs.len(),
                "candidates": candidates,
            }),
        };
        Ok(envelope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Test runner that returns a fixed canned set of outputs.
    struct CannedRunner {
        outputs: Mutex<Option<Vec<WorkerOutput>>>,
    }
    impl CannedRunner {
        fn new(outputs: Vec<WorkerOutput>) -> Self {
            Self {
                outputs: Mutex::new(Some(outputs)),
            }
        }
    }
    #[async_trait]
    impl WorkerRunner for CannedRunner {
        async fn run(
            &self,
            _prompt: &str,
            _n_agents: usize,
        ) -> Result<Vec<WorkerOutput>, anyhow::Error> {
            self.outputs
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| anyhow::anyhow!("canned runner already drained"))
        }
    }

    #[tokio::test]
    async fn p13_14_quorum_when_majority_agrees() {
        let runner = Arc::new(CannedRunner::new(vec![
            WorkerOutput {
                agent_id: "w1".into(),
                output: json!({"answer": "yes"}),
            },
            WorkerOutput {
                agent_id: "w2".into(),
                output: json!({"answer": "yes"}),
            },
            WorkerOutput {
                agent_id: "w3".into(),
                output: json!({"answer": "no"}),
            },
        ]));
        let pool = WorkerPool::new(runner);
        let res = pool.call(&json!({"prompt": "do thing"})).await.unwrap();
        assert_eq!(res["consensus"], "quorum");
        assert_eq!(res["winner"]["answer"], "yes");
        assert_eq!(res["votes"], 2);
    }

    #[tokio::test]
    async fn p13_14_no_quorum_when_split() {
        let runner = Arc::new(CannedRunner::new(vec![
            WorkerOutput {
                agent_id: "w1".into(),
                output: json!({"answer": "a"}),
            },
            WorkerOutput {
                agent_id: "w2".into(),
                output: json!({"answer": "b"}),
            },
            WorkerOutput {
                agent_id: "w3".into(),
                output: json!({"answer": "c"}),
            },
        ]));
        let pool = WorkerPool::new(runner);
        let res = pool
            .call(&json!({"prompt": "split decision"}))
            .await
            .unwrap();
        assert_eq!(res["consensus"], "none");
        assert_eq!(res["top_votes"], 1);
    }

    #[tokio::test]
    async fn p13_14_returns_no_workers_envelope_when_runner_empty() {
        let runner = Arc::new(CannedRunner::new(vec![]));
        let pool = WorkerPool::new(runner);
        let res = pool.call(&json!({"prompt": "x"})).await.unwrap();
        assert_eq!(res["consensus"], "none");
        assert_eq!(res["reason"], "no worker returned output");
    }

    #[tokio::test]
    async fn p13_14_missing_prompt_errors() {
        let runner = Arc::new(CannedRunner::new(vec![]));
        let pool = WorkerPool::new(runner);
        assert!(pool.call(&json!({})).await.is_err());
    }

    #[tokio::test]
    async fn p13_14_n_agents_override_propagates() {
        struct CountingRunner {
            seen: Mutex<usize>,
        }
        #[async_trait]
        impl WorkerRunner for CountingRunner {
            async fn run(
                &self,
                _prompt: &str,
                n: usize,
            ) -> Result<Vec<WorkerOutput>, anyhow::Error> {
                *self.seen.lock().unwrap() = n;
                Ok(vec![WorkerOutput {
                    agent_id: "only".into(),
                    output: json!({"answer": "ok"}),
                }])
            }
        }
        let runner = Arc::new(CountingRunner {
            seen: Mutex::new(0),
        });
        let pool = WorkerPool::new(runner.clone()).with_default_n(2);
        let _ = pool
            .call(&json!({"prompt": "x", "n_agents": 7}))
            .await
            .unwrap();
        assert_eq!(*runner.seen.lock().unwrap(), 7);
    }

    #[tokio::test]
    async fn p13_14_default_n_used_when_unspecified() {
        struct CountingRunner {
            seen: Mutex<usize>,
        }
        #[async_trait]
        impl WorkerRunner for CountingRunner {
            async fn run(
                &self,
                _prompt: &str,
                n: usize,
            ) -> Result<Vec<WorkerOutput>, anyhow::Error> {
                *self.seen.lock().unwrap() = n;
                Ok(vec![])
            }
        }
        let runner = Arc::new(CountingRunner {
            seen: Mutex::new(0),
        });
        let pool = WorkerPool::new(runner.clone()).with_default_n(5);
        let _ = pool.call(&json!({"prompt": "x"})).await.unwrap();
        assert_eq!(*runner.seen.lock().unwrap(), 5);
    }
}
