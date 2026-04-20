//! Memory extraction pipeline.
//!
//! Periodically distills durable facts, preferences and conventions from
//! conversation turns and persists them to `.caduceus/memory.md`, which
//! is loaded into the system prompt by `instructions.rs`.
//!
//! Equivalent to Claude Code's `extractMemories` service: a separate LLM
//! call summarises the conversation into bullet-list "remember-this"
//! items. The extractor is wired through a trait so unit tests can
//! inject deterministic distillers without making real API calls.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use caduceus_providers::Message;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("distiller error: {0}")]
    Distiller(String),
    #[error("nothing to extract")]
    Empty,
}

/// Categorical bucket for an extracted memory. Keeps the persisted
/// markdown grouped and makes future filtering possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryCategory {
    Preference,
    Fact,
    Convention,
    Skill,
    Other,
}

impl MemoryCategory {
    pub fn label(&self) -> &'static str {
        match self {
            MemoryCategory::Preference => "preference",
            MemoryCategory::Fact => "fact",
            MemoryCategory::Convention => "convention",
            MemoryCategory::Skill => "skill",
            MemoryCategory::Other => "other",
        }
    }

    fn from_label(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "preference" | "pref" => Self::Preference,
            "fact" => Self::Fact,
            "convention" | "style" => Self::Convention,
            "skill" => Self::Skill,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedMemory {
    pub category: MemoryCategory,
    pub content: String,
}

/// Trait abstracting the LLM-backed distillation step. Production code
/// uses `ProviderDistiller`; tests use a mock that returns a fixed list.
#[async_trait]
pub trait MemoryDistiller: Send + Sync {
    async fn distill(&self, transcript: &str) -> Result<Vec<ExtractedMemory>, MemoryError>;
}

/// Persistent on-disk store. Backed by `.caduceus/memory.md`.
///
/// The file is human-editable markdown. We round-trip our entries as
/// `- [category] content` lines so users can also hand-edit them; any
/// non-matching `-` line is treated as `Other` on read.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    path: PathBuf,
    /// Hard cap on how many entries we keep on disk. When exceeded we
    /// drop the oldest entries (FIFO) so the system prompt does not
    /// balloon over time.
    max_entries: usize,
}

impl MemoryStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            max_entries: 200,
        }
    }

    pub fn with_max_entries(mut self, max: usize) -> Self {
        self.max_entries = max.max(1);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn read_all(&self) -> Result<Vec<ExtractedMemory>, MemoryError> {
        let raw = match fs::read_to_string(&self.path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(MemoryError::Io(e)),
        };
        Ok(parse_markdown(&raw))
    }

    /// Append `new_entries`, skipping any whose normalized content
    /// already exists. Returns the count of entries that were actually
    /// written. Atomic via write-to-temp + rename.
    pub async fn append_unique(
        &self,
        new_entries: &[ExtractedMemory],
    ) -> Result<usize, MemoryError> {
        if new_entries.is_empty() {
            return Ok(0);
        }
        let mut existing = self.read_all().await?;
        let mut seen: HashSet<String> = existing
            .iter()
            .map(|m| normalize_content(&m.content))
            .collect();

        let mut added = 0usize;
        for m in new_entries {
            let trimmed = m.content.trim();
            if trimmed.is_empty() {
                continue;
            }
            let key = normalize_content(trimmed);
            if seen.insert(key) {
                existing.push(ExtractedMemory {
                    category: m.category,
                    content: trimmed.to_string(),
                });
                added += 1;
            }
        }

        if added == 0 {
            return Ok(0);
        }

        // FIFO trim
        if existing.len() > self.max_entries {
            let drop_n = existing.len() - self.max_entries;
            existing.drain(0..drop_n);
        }

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let tmp = self.path.with_extension("md.tmp");
        fs::write(&tmp, render_markdown(&existing)).await?;
        fs::rename(&tmp, &self.path).await?;
        Ok(added)
    }
}

/// High-level extraction service. Combines a transcript renderer, the
/// LLM-backed distiller, and the on-disk store.
pub struct MemoryExtractor<D: MemoryDistiller> {
    distiller: D,
    store: MemoryStore,
    /// Minimum number of messages before extraction runs; prevents
    /// thrashing on every short reply.
    min_messages: usize,
    /// Cap on transcript characters sent to the distiller, to bound
    /// token cost. The most-recent suffix is kept.
    max_transcript_chars: usize,
}

impl<D: MemoryDistiller> MemoryExtractor<D> {
    pub fn new(distiller: D, store: MemoryStore) -> Self {
        Self {
            distiller,
            store,
            min_messages: 6,
            max_transcript_chars: 24_000,
        }
    }

    pub fn min_messages(mut self, n: usize) -> Self {
        self.min_messages = n;
        self
    }

    pub fn max_transcript_chars(mut self, n: usize) -> Self {
        self.max_transcript_chars = n.max(512);
        self
    }

    /// Extract memories from `messages` and append unique ones to the
    /// store. Returns the entries that were newly persisted.
    pub async fn extract_and_persist(
        &self,
        messages: &[Message],
    ) -> Result<Vec<ExtractedMemory>, MemoryError> {
        if messages.len() < self.min_messages {
            return Ok(Vec::new());
        }
        let transcript = render_transcript(messages, self.max_transcript_chars);
        if transcript.trim().is_empty() {
            return Ok(Vec::new());
        }
        let candidates = self.distiller.distill(&transcript).await?;
        let mut filtered = Vec::with_capacity(candidates.len());
        let mut seen_in_batch = HashSet::new();
        for c in candidates {
            let trimmed = c.content.trim();
            if trimmed.len() < 3 {
                continue;
            }
            let key = normalize_content(trimmed);
            if seen_in_batch.insert(key) {
                filtered.push(ExtractedMemory {
                    category: c.category,
                    content: trimmed.to_string(),
                });
            }
        }
        if filtered.is_empty() {
            return Ok(Vec::new());
        }
        // Snapshot existing keys BEFORE append so we can report only the
        // entries that were actually newly persisted.
        let pre_existing: HashSet<String> = self
            .store
            .read_all()
            .await?
            .iter()
            .map(|m| normalize_content(&m.content))
            .collect();
        let _added = self.store.append_unique(&filtered).await?;
        Ok(filtered
            .into_iter()
            .filter(|m| !pre_existing.contains(&normalize_content(&m.content)))
            .collect())
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn normalize_content(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn render_transcript(messages: &[Message], max_chars: usize) -> String {
    let mut out = String::new();
    for m in messages {
        if m.content.trim().is_empty() {
            continue;
        }
        out.push_str(&m.role);
        out.push_str(": ");
        out.push_str(m.content.trim());
        out.push_str("\n\n");
    }
    if out.len() > max_chars {
        let cut = out.len() - max_chars;
        // Find a UTF-8 char boundary at or after `cut`.
        let mut idx = cut;
        while idx < out.len() && !out.is_char_boundary(idx) {
            idx += 1;
        }
        out = out[idx..].to_string();
    }
    out
}

fn render_markdown(entries: &[ExtractedMemory]) -> String {
    let mut out = String::from("# Caduceus Memory\n\n");
    out.push_str("<!-- Auto-curated by extractMemories. Hand-editing OK. -->\n\n");
    for e in entries {
        out.push_str("- [");
        out.push_str(e.category.label());
        out.push_str("] ");
        out.push_str(e.content.trim());
        out.push('\n');
    }
    out
}

fn parse_markdown(raw: &str) -> Vec<ExtractedMemory> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('-') {
            continue;
        }
        let body = trimmed.trim_start_matches('-').trim();
        if body.is_empty() {
            continue;
        }
        let (category, content) = if let Some(rest) = body.strip_prefix('[') {
            if let Some(end) = rest.find(']') {
                let label = &rest[..end];
                let after = rest[end + 1..].trim();
                (MemoryCategory::from_label(label), after.to_string())
            } else {
                (MemoryCategory::Other, body.to_string())
            }
        } else {
            (MemoryCategory::Other, body.to_string())
        };
        if !content.is_empty() {
            out.push(ExtractedMemory { category, content });
        }
    }
    out
}

/// Convenience adapter that wraps an async closure into a
/// `MemoryDistiller`. Lets call sites plug in any LLM client without
/// implementing the trait by hand.
///
/// ```ignore
/// let distiller = FnDistiller::new(|transcript| async move {
///     // call provider, parse JSON response, return Vec<ExtractedMemory>
///     Ok(vec![])
/// });
/// ```
pub struct FnDistiller<F> {
    f: F,
}

impl<F> FnDistiller<F> {
    pub fn new(f: F) -> Self {
        Self { f }
    }
}

#[async_trait]
impl<F, Fut> MemoryDistiller for FnDistiller<F>
where
    F: Fn(String) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<Vec<ExtractedMemory>, MemoryError>> + Send,
{
    async fn distill(&self, transcript: &str) -> Result<Vec<ExtractedMemory>, MemoryError> {
        (self.f)(transcript.to_string()).await
    }
}

/// Default extraction prompt suitable for OpenAI/Anthropic-compatible
/// chat completion APIs. Asks the model to respond with a JSON array
/// of `{category, content}` objects. Callers that need a different
/// schema can supply their own prompt via [`FnDistiller`].
pub const DEFAULT_EXTRACTION_PROMPT: &str = r#"You are a memory-distillation assistant.
Read the conversation transcript and extract DURABLE facts that would be useful
to remember in future sessions: user preferences, project conventions, key
facts about the codebase, and learned skills.

Rules:
- Skip ephemeral/situational details (today's bug, current file path, etc.).
- Each entry must be self-contained — no pronouns referring to the transcript.
- Maximum 5 entries. Each ≤ 140 characters.
- Respond ONLY with a JSON array. No prose, no markdown fences.

Schema: [{"category": "preference"|"fact"|"convention"|"skill", "content": "string"}]
"#;

/// Parse a model JSON response into `ExtractedMemory` entries. Tolerates
/// surrounding whitespace and Markdown code-fence wrappers that some
/// models emit despite instructions. Returns an empty Vec if parsing
/// fails — extraction is best-effort and must never crash the agent.
pub fn parse_distiller_json(raw: &str) -> Vec<ExtractedMemory> {
    let cleaned = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    #[derive(Deserialize)]
    struct Entry {
        category: Option<String>,
        content: String,
    }
    let parsed: Result<Vec<Entry>, _> = serde_json::from_str(cleaned);
    match parsed {
        Ok(items) => items
            .into_iter()
            .filter_map(|e| {
                let content = e.content.trim().to_string();
                if content.is_empty() {
                    return None;
                }
                let category = e
                    .category
                    .map(|c| MemoryCategory::from_label(&c))
                    .unwrap_or(MemoryCategory::Other);
                Some(ExtractedMemory { category, content })
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    fn msg(role: &str, content: &str) -> Message {
        Message {
            role: role.to_string(),
            content: content.to_string(),
            content_blocks: None,
            tool_calls: Vec::new(),
            tool_result: None,
            cache_breakpoint: false,
        }
    }

    /// Mock distiller that returns a pre-set list and records the
    /// transcripts it received.
    struct MockDistiller {
        next: Mutex<Vec<Vec<ExtractedMemory>>>,
        transcripts: Arc<Mutex<Vec<String>>>,
    }

    impl MockDistiller {
        fn new(batches: Vec<Vec<ExtractedMemory>>) -> Self {
            Self {
                next: Mutex::new(batches),
                transcripts: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MemoryDistiller for MockDistiller {
        async fn distill(&self, transcript: &str) -> Result<Vec<ExtractedMemory>, MemoryError> {
            self.transcripts
                .lock()
                .unwrap()
                .push(transcript.to_string());
            let mut q = self.next.lock().unwrap();
            if q.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(q.remove(0))
            }
        }
    }

    fn em(cat: MemoryCategory, content: &str) -> ExtractedMemory {
        ExtractedMemory {
            category: cat,
            content: content.to_string(),
        }
    }

    #[tokio::test]
    async fn store_round_trip_preserves_categories() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::new(tmp.path().join("memory.md"));
        let entries = vec![
            em(MemoryCategory::Preference, "Prefer async/await"),
            em(MemoryCategory::Fact, "Repo uses cargo workspace"),
        ];
        let added = store.append_unique(&entries).await.unwrap();
        assert_eq!(added, 2);
        let back = store.read_all().await.unwrap();
        assert_eq!(back, entries);
    }

    #[tokio::test]
    async fn store_dedupes_case_and_whitespace() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::new(tmp.path().join("memory.md"));
        store
            .append_unique(&[em(MemoryCategory::Fact, "Use rustls")])
            .await
            .unwrap();
        let added = store
            .append_unique(&[
                em(MemoryCategory::Fact, "use   RUSTLS"),
                em(MemoryCategory::Convention, "Use rustls\n"),
            ])
            .await
            .unwrap();
        assert_eq!(added, 0, "duplicate variants must be skipped");
        let back = store.read_all().await.unwrap();
        assert_eq!(back.len(), 1);
    }

    #[tokio::test]
    async fn store_fifo_trims_to_max() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::new(tmp.path().join("memory.md")).with_max_entries(3);
        for i in 0..5 {
            store
                .append_unique(&[em(MemoryCategory::Fact, &format!("entry-{i}"))])
                .await
                .unwrap();
        }
        let back = store.read_all().await.unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].content, "entry-2"); // 0,1 dropped
        assert_eq!(back[2].content, "entry-4");
    }

    #[tokio::test]
    async fn store_atomic_write_creates_parent() {
        let tmp = TempDir::new().unwrap();
        let nested = tmp.path().join("a/b/c/memory.md");
        let store = MemoryStore::new(&nested);
        store
            .append_unique(&[em(MemoryCategory::Fact, "x")])
            .await
            .unwrap();
        assert!(nested.exists());
        // No tmp file lingering.
        assert!(!nested.with_extension("md.tmp").exists());
    }

    #[tokio::test]
    async fn extractor_skips_when_below_min_messages() {
        let tmp = TempDir::new().unwrap();
        let mock = MockDistiller::new(vec![vec![em(MemoryCategory::Fact, "should-not-appear")]]);
        let transcripts = mock.transcripts.clone();
        let store = MemoryStore::new(tmp.path().join("memory.md"));
        let extractor = MemoryExtractor::new(mock, store.clone()).min_messages(6);

        let new = extractor
            .extract_and_persist(&[msg("user", "hi"), msg("assistant", "hello")])
            .await
            .unwrap();
        assert!(new.is_empty());
        assert!(
            transcripts.lock().unwrap().is_empty(),
            "must NOT call distiller"
        );
        assert!(store.read_all().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn extractor_persists_unique_entries() {
        let tmp = TempDir::new().unwrap();
        let mock = MockDistiller::new(vec![vec![
            em(MemoryCategory::Preference, "Prefer concise replies"),
            em(MemoryCategory::Fact, "Project name is Caduceus"),
            em(MemoryCategory::Fact, ""), // dropped (empty)
        ]]);
        let store = MemoryStore::new(tmp.path().join("memory.md"));
        let extractor = MemoryExtractor::new(mock, store.clone()).min_messages(2);

        let messages: Vec<Message> = (0..6)
            .map(|i| {
                msg(
                    if i % 2 == 0 { "user" } else { "assistant" },
                    &format!("turn {i}"),
                )
            })
            .collect();
        let new = extractor.extract_and_persist(&messages).await.unwrap();
        assert_eq!(new.len(), 2);
        let stored = store.read_all().await.unwrap();
        assert_eq!(stored.len(), 2);
    }

    #[tokio::test]
    async fn extractor_dedupes_against_existing_store() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::new(tmp.path().join("memory.md"));
        store
            .append_unique(&[em(MemoryCategory::Fact, "Repo uses tokio")])
            .await
            .unwrap();

        let mock = MockDistiller::new(vec![vec![
            em(MemoryCategory::Fact, "Repo uses TOKIO"), // dup
            em(MemoryCategory::Convention, "Use #[must_use] on builders"),
        ]]);
        let extractor = MemoryExtractor::new(mock, store.clone()).min_messages(2);
        let messages: Vec<Message> = (0..4).map(|i| msg("user", &format!("m{i}"))).collect();
        let new = extractor.extract_and_persist(&messages).await.unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].content, "Use #[must_use] on builders");
        assert_eq!(store.read_all().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn extractor_truncates_long_transcript_at_char_boundary() {
        let tmp = TempDir::new().unwrap();
        let mock = MockDistiller::new(vec![vec![]]);
        let transcripts = mock.transcripts.clone();
        let store = MemoryStore::new(tmp.path().join("memory.md"));
        let extractor = MemoryExtractor::new(mock, store)
            .min_messages(2)
            .max_transcript_chars(512);

        // Multi-byte chars to ensure the boundary logic does not panic.
        let long = "✨".repeat(2_000);
        let messages = vec![
            msg("user", &long),
            msg("assistant", &long),
            msg("user", "ok"),
        ];
        let _ = extractor.extract_and_persist(&messages).await.unwrap();
        let t = transcripts.lock().unwrap();
        assert_eq!(t.len(), 1);
        assert!(
            t[0].len() <= 512 + 8,
            "transcript not truncated: {}",
            t[0].len()
        );
    }

    #[tokio::test]
    async fn parse_handles_legacy_markdown_without_category() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("memory.md");
        fs::write(&path, "# Memory\n- legacy line one\n- [pref] new style\n")
            .await
            .unwrap();
        let store = MemoryStore::new(&path);
        let entries = store.read_all().await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].category, MemoryCategory::Other);
        assert_eq!(entries[0].content, "legacy line one");
        assert_eq!(entries[1].category, MemoryCategory::Preference);
    }

    #[tokio::test]
    async fn extractor_propagates_distiller_errors() {
        struct Failing;
        #[async_trait]
        impl MemoryDistiller for Failing {
            async fn distill(&self, _: &str) -> Result<Vec<ExtractedMemory>, MemoryError> {
                Err(MemoryError::Distiller("rate limit".into()))
            }
        }
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::new(tmp.path().join("memory.md"));
        let extractor = MemoryExtractor::new(Failing, store).min_messages(2);
        let messages: Vec<Message> = (0..4).map(|i| msg("user", &format!("m{i}"))).collect();
        let err = extractor.extract_and_persist(&messages).await.unwrap_err();
        assert!(matches!(err, MemoryError::Distiller(_)));
    }

    #[test]
    fn parse_json_accepts_plain_array() {
        let out = parse_distiller_json(
            r#"[{"category":"preference","content":"Use rustfmt"},
                {"category":"fact","content":"Repo is Rust"}]"#,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].category, MemoryCategory::Preference);
        assert_eq!(out[1].content, "Repo is Rust");
    }

    #[test]
    fn parse_json_strips_code_fences_and_unknown_categories() {
        let out =
            parse_distiller_json("```json\n[{\"category\":\"weird\",\"content\":\"x\"}]\n```");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].category, MemoryCategory::Other);
    }

    #[test]
    fn parse_json_returns_empty_on_garbage() {
        assert!(parse_distiller_json("not json at all").is_empty());
        assert!(parse_distiller_json("").is_empty());
    }

    #[test]
    fn parse_json_drops_empty_content() {
        let out = parse_distiller_json(r#"[{"category":"fact","content":"   "}]"#);
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn fn_distiller_round_trip() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::new(tmp.path().join("memory.md"));
        let distiller = FnDistiller::new(|_transcript: String| async move {
            Ok(parse_distiller_json(
                r#"[{"category":"fact","content":"closure works"}]"#,
            ))
        });
        let extractor = MemoryExtractor::new(distiller, store.clone()).min_messages(2);
        let messages: Vec<Message> = (0..4).map(|i| msg("user", &format!("m{i}"))).collect();
        let new = extractor.extract_and_persist(&messages).await.unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].content, "closure works");
    }
}
