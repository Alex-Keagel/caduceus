//! P13.11 — Tool retrieval / ranking layer (G‑R6.5).
//!
//! When the tool registry grows beyond a threshold (default 30), feeding the
//! full schema for every tool into the system prompt wastes tokens and dilutes
//! the LLM's attention. Instead, we ship a compact *tool digest* (one line per
//! tool: `name — description`) plus a `tool_search` meta‑tool that returns the
//! top‑K matches for a natural‑language query. The agent calls `tool_search`
//! lazily, then receives the full schema only for the tools it actually needs.
//!
//! Ranking uses a simple BM25‑style TF‑IDF over the tool name + description.
//! Cite: Qin et al., *ToolLLM: Facilitating Large Language Models to Master
//! 16000+ Real‑World APIs* (ICLR 2024, arXiv:2307.16789).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single retrievable tool descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
}

/// A ranked search hit.
#[derive(Debug, Clone)]
pub struct ToolHit {
    pub name: String,
    pub description: String,
    pub score: f32,
}

/// Default threshold above which the digest+retrieval path activates.
pub const DEFAULT_DIGEST_THRESHOLD: usize = 30;

/// Render a one‑line‑per‑tool digest. Stable order = input order.
/// Each line: `- <name>: <description-up-to-100-chars>`.
pub fn render_digest(tools: &[ToolDescriptor]) -> String {
    let mut s = String::new();
    s.push_str("Available tools (use `tool_search` for full schemas):\n");
    for t in tools {
        let desc = t.description.chars().take(100).collect::<String>();
        s.push_str(&format!("- {}: {}\n", t.name, desc));
    }
    s
}

fn tokenise(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect()
}

/// Rank `tools` against `query` using a small BM25 variant. Returns the top
/// `k` hits sorted by descending score. Tools with score 0 are excluded.
pub fn rank(tools: &[ToolDescriptor], query: &str, k: usize) -> Vec<ToolHit> {
    if tools.is_empty() || k == 0 {
        return Vec::new();
    }
    let q_terms = tokenise(query);
    if q_terms.is_empty() {
        return Vec::new();
    }
    // Document frequency per query term across the corpus.
    let mut df: HashMap<&str, usize> = HashMap::new();
    let docs: Vec<Vec<String>> = tools
        .iter()
        .map(|t| {
            let combined = format!("{} {}", t.name, t.description);
            tokenise(&combined)
        })
        .collect();
    for doc in &docs {
        for term in &q_terms {
            if doc.iter().any(|w| w == term) {
                *df.entry(term.as_str()).or_insert(0) += 1;
            }
        }
    }
    let n = tools.len() as f32;
    let avgdl: f32 = docs.iter().map(|d| d.len() as f32).sum::<f32>() / n.max(1.0);
    let k1 = 1.5_f32;
    let b = 0.75_f32;

    let mut hits: Vec<ToolHit> = tools
        .iter()
        .zip(docs.iter())
        .map(|(t, doc)| {
            let dl = doc.len() as f32;
            let mut score = 0.0_f32;
            for term in &q_terms {
                let f = doc.iter().filter(|w| *w == term).count() as f32;
                if f == 0.0 {
                    continue;
                }
                let nq = *df.get(term.as_str()).unwrap_or(&0) as f32;
                let idf = ((n - nq + 0.5) / (nq + 0.5) + 1.0).ln();
                let denom = f + k1 * (1.0 - b + b * dl / avgdl.max(1.0));
                score += idf * (f * (k1 + 1.0)) / denom.max(1e-6);
            }
            // Name‑hit bonus: heavy weight when query term appears in name.
            let name_terms = tokenise(&t.name);
            let name_hits = q_terms
                .iter()
                .filter(|q| name_terms.iter().any(|n| n == *q))
                .count();
            score += 2.0 * name_hits as f32;
            ToolHit {
                name: t.name.clone(),
                description: t.description.clone(),
                score,
            }
        })
        .filter(|h| h.score > 0.0)
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    hits.truncate(k);
    hits
}

/// Build a JSON spec for the `tool_search` meta‑tool. Inject this alongside
/// the digest when the registry is large.
pub fn tool_search_spec() -> serde_json::Value {
    serde_json::json!({
        "name": "tool_search",
        "description": "Search the tool registry by natural-language query. Returns the top-K matching tools with their full schemas. Use this when you need a tool that wasn't in the abbreviated digest.",
        "input_schema": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Natural-language description of the capability you need."},
                "k": {"type": "integer", "default": 5, "description": "Maximum number of hits to return."}
            },
            "required": ["query"]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<ToolDescriptor> {
        vec![
            ToolDescriptor {
                name: "read_file".into(),
                description: "Read the contents of a file from disk.".into(),
            },
            ToolDescriptor {
                name: "write_file".into(),
                description: "Write a string to a file on disk, replacing any existing content."
                    .into(),
            },
            ToolDescriptor {
                name: "shell".into(),
                description: "Execute a bash command in the project root.".into(),
            },
            ToolDescriptor {
                name: "grep".into(),
                description: "Search for a pattern across files using ripgrep.".into(),
            },
            ToolDescriptor {
                name: "lsp_definition".into(),
                description: "Jump to the definition of a symbol via the language server.".into(),
            },
        ]
    }

    #[test]
    fn p13_11_digest_renders_one_line_per_tool() {
        let s = render_digest(&fixture());
        assert!(s.starts_with("Available tools"));
        assert_eq!(s.matches("\n- ").count(), 5);
    }

    #[test]
    fn p13_11_rank_returns_topk_for_relevant_query() {
        let hits = rank(&fixture(), "search for a string in files", 3);
        assert_eq!(hits[0].name, "grep");
        assert!(hits.len() <= 3);
    }

    #[test]
    fn p13_11_rank_prefers_name_match() {
        let hits = rank(&fixture(), "shell", 5);
        assert_eq!(hits[0].name, "shell");
    }

    #[test]
    fn p13_11_rank_excludes_zero_score() {
        let hits = rank(&fixture(), "antarctic penguin", 5);
        assert!(hits.is_empty());
    }

    #[test]
    fn p13_11_rank_empty_inputs_safe() {
        assert!(rank(&[], "anything", 5).is_empty());
        assert!(rank(&fixture(), "", 5).is_empty());
        assert!(rank(&fixture(), "shell", 0).is_empty());
    }

    #[test]
    fn p13_11_tool_search_spec_well_formed() {
        let spec = tool_search_spec();
        assert_eq!(spec["name"], "tool_search");
        assert!(spec["input_schema"]["properties"]["query"].is_object());
    }

    #[test]
    fn p13_11_threshold_constant_is_30() {
        assert_eq!(DEFAULT_DIGEST_THRESHOLD, 30);
    }

    #[test]
    fn p13_11_rank_ties_break_by_name() {
        // Two tools with identical descriptions; query matches both equally.
        let tools = vec![
            ToolDescriptor {
                name: "zeta".into(),
                description: "fooz bar".into(),
            },
            ToolDescriptor {
                name: "alpha".into(),
                description: "fooz bar".into(),
            },
        ];
        let hits = rank(&tools, "fooz", 5);
        assert_eq!(hits[0].name, "alpha");
        assert_eq!(hits[1].name, "zeta");
    }
}
